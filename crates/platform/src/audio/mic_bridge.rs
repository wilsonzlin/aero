//! SharedArrayBuffer ring-buffer layout used to capture microphone PCM samples.
//!
//! The browser-side `AudioWorkletProcessor` (or the main-thread fallback) is the
//! producer, and the emulator is the consumer.
//!
//! Indices are stored as monotonically increasing `u32` sample counters
//! (wrapping naturally at `2^32`) to avoid the classic "read == write" ambiguity.

/// Header layout (`Uint32Array`) in the SharedArrayBuffer.
pub const HEADER_U32_LEN: usize = 4;

/// Total samples written by the producer (monotonic, wraps at `2^32`).
pub const WRITE_POS_INDEX: usize = 0;
/// Total samples read by the consumer.
pub const READ_POS_INDEX: usize = 1;
/// Total samples dropped due to buffer pressure.
pub const DROPPED_SAMPLES_INDEX: usize = 2;
/// Ring buffer capacity in samples (constant).
pub const CAPACITY_SAMPLES_INDEX: usize = 3;

/// Total bytes reserved for the header.
pub const HEADER_BYTES: usize = HEADER_U32_LEN * 4;

/// Maximum microphone ring capacity supported by the mic bridge helpers.
///
/// The mic capture ring is host-driven and can be backed by a `SharedArrayBuffer` in the browser
/// or an in-memory `Vec<f32>` in the pure-Rust test helper. Even in non-wasm builds, callers may
/// treat this as untrusted input (e.g. from UI/config), so we clamp to a reasonable upper bound to
/// avoid multi-gigabyte allocations.
///
/// `2^20` mono samples is ~21s at 48kHz and ~4MiB of f32 storage.
const MAX_RING_CAPACITY_SAMPLES: u32 = 1_048_576;

#[inline]
pub fn samples_available(read_pos: u32, write_pos: u32) -> u32 {
    write_pos.wrapping_sub(read_pos)
}

#[inline]
pub fn samples_available_clamped(read_pos: u32, write_pos: u32, capacity_samples: u32) -> u32 {
    samples_available(read_pos, write_pos).min(capacity_samples)
}

#[inline]
pub fn samples_free(read_pos: u32, write_pos: u32, capacity_samples: u32) -> u32 {
    capacity_samples - samples_available_clamped(read_pos, write_pos, capacity_samples)
}

/// A small, pure-Rust mono ring buffer used for unit testing index math and wrap-around.
///
/// This mirrors the browser producer behaviour: when the buffer is under pressure
/// it drops the oldest part of the *current block* (keeps the most recent
/// samples) to bias for low latency.
#[derive(Debug)]
pub struct MonoRingBuffer {
    capacity_samples: u32,
    read_pos: u32,
    write_pos: u32,
    dropped_samples: u64,
    storage: Vec<f32>,
}

impl MonoRingBuffer {
    pub fn new(capacity_samples: u32) -> Self {
        assert!(capacity_samples > 0, "capacity_samples must be non-zero");
        let capacity_samples = capacity_samples.min(MAX_RING_CAPACITY_SAMPLES);
        Self {
            capacity_samples,
            read_pos: 0,
            write_pos: 0,
            dropped_samples: 0,
            storage: vec![0.0; capacity_samples as usize],
        }
    }

    pub fn buffered_samples(&self) -> u32 {
        samples_available_clamped(self.read_pos, self.write_pos, self.capacity_samples)
    }

    pub fn take_dropped_samples_delta(&mut self) -> u64 {
        let dropped = self.dropped_samples;
        self.dropped_samples = 0;
        dropped
    }

    /// Write a block of samples.
    ///
    /// Returns the number of samples written. If the buffer is full, this
    /// returns 0 without blocking.
    pub fn write(&mut self, samples: &[f32]) -> u32 {
        let requested = samples.len() as u32;
        if requested == 0 {
            return 0;
        }

        let used = samples_available(self.read_pos, self.write_pos);
        if used > self.capacity_samples {
            // Consumer fell behind far enough that we no longer know what's valid.
            // Drop this block to avoid making things worse (mirrors JS worklet).
            self.dropped_samples = self.dropped_samples.saturating_add(samples.len() as u64);
            return 0;
        }

        let free = self.capacity_samples - used;
        if free == 0 {
            self.dropped_samples = self.dropped_samples.saturating_add(samples.len() as u64);
            return 0;
        }

        let to_write = requested.min(free);
        let dropped = requested - to_write;
        self.dropped_samples = self.dropped_samples.saturating_add(dropped as u64);

        // Keep the most recent part of the block if we have to drop.
        let slice = if dropped > 0 {
            &samples[dropped as usize..]
        } else {
            samples
        };

        let start = self.write_pos % self.capacity_samples;
        let first_part = to_write.min(self.capacity_samples - start);
        let second_part = to_write - first_part;

        let start_usize = start as usize;
        let first_usize = first_part as usize;
        self.storage[start_usize..start_usize + first_usize].copy_from_slice(&slice[..first_usize]);

        if second_part > 0 {
            let second_usize = second_part as usize;
            self.storage[..second_usize]
                .copy_from_slice(&slice[first_usize..first_usize + second_usize]);
        }

        self.write_pos = self.write_pos.wrapping_add(to_write);
        to_write
    }

    /// Read samples into `out`.
    ///
    /// Returns the number of samples read. If the buffer is empty, returns 0
    /// without blocking.
    pub fn read(&mut self, out: &mut [f32]) -> u32 {
        let requested = out.len() as u32;
        if requested == 0 {
            return 0;
        }

        let available =
            samples_available_clamped(self.read_pos, self.write_pos, self.capacity_samples);
        let to_read = requested.min(available);
        if to_read == 0 {
            return 0;
        }

        let start = self.read_pos % self.capacity_samples;
        let first_part = to_read.min(self.capacity_samples - start);
        let second_part = to_read - first_part;

        let start_usize = start as usize;
        let first_usize = first_part as usize;
        out[..first_usize].copy_from_slice(&self.storage[start_usize..start_usize + first_usize]);

        if second_part > 0 {
            let second_usize = second_part as usize;
            out[first_usize..first_usize + second_usize]
                .copy_from_slice(&self.storage[..second_usize]);
        }

        self.read_pos = self.read_pos.wrapping_add(to_read);
        to_read
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_samples_available_wraps_u32() {
        let read = u32::MAX - 10;
        let write = read.wrapping_add(5);
        assert_eq!(samples_available(read, write), 5);
    }

    #[test]
    fn test_new_clamps_excessive_capacity_to_avoid_oom() {
        let rb = MonoRingBuffer::new(u32::MAX);
        assert_eq!(rb.capacity_samples, MAX_RING_CAPACITY_SAMPLES);
        assert_eq!(rb.storage.len(), MAX_RING_CAPACITY_SAMPLES as usize);
    }

    #[test]
    fn test_ring_buffer_wrap_around_preserves_order() {
        let mut rb = MonoRingBuffer::new(4);

        let written = rb.write(&[0.0, 1.0, 2.0]);
        assert_eq!(written, 3);
        assert_eq!(rb.buffered_samples(), 3);

        let mut out = [0.0f32; 2];
        let read = rb.read(&mut out);
        assert_eq!(read, 2);
        assert_eq!(out, [0.0, 1.0]);
        assert_eq!(rb.buffered_samples(), 1);

        let written = rb.write(&[3.0, 4.0, 5.0]);
        assert_eq!(written, 3);
        assert_eq!(rb.buffered_samples(), 4);

        let mut out = [0.0f32; 4];
        let read = rb.read(&mut out);
        assert_eq!(read, 4);
        assert_eq!(out, [2.0, 3.0, 4.0, 5.0]);
        assert_eq!(rb.buffered_samples(), 0);
    }

    #[test]
    fn test_partial_write_keeps_most_recent_samples_of_block() {
        let mut rb = MonoRingBuffer::new(4);

        let written = rb.write(&[0.0, 1.0, 2.0]);
        assert_eq!(written, 3);

        // Only 1 sample of free space remains. The block [3,4,5] should be
        // partially written by keeping the most recent sample (5) and dropping
        // the older part of the block (3,4).
        let written = rb.write(&[3.0, 4.0, 5.0]);
        assert_eq!(written, 1);
        assert_eq!(rb.take_dropped_samples_delta(), 2);

        let mut out = [0.0f32; 4];
        let read = rb.read(&mut out);
        assert_eq!(read, 4);
        assert_eq!(out, [0.0, 1.0, 2.0, 5.0]);
    }

    #[test]
    fn test_dropped_samples_counts_full_block_drop_when_full() {
        let mut rb = MonoRingBuffer::new(4);

        let written = rb.write(&[0.0, 1.0, 2.0, 3.0]);
        assert_eq!(written, 4);
        assert_eq!(rb.take_dropped_samples_delta(), 0);

        let written = rb.write(&[4.0, 5.0]);
        assert_eq!(written, 0);
        assert_eq!(rb.take_dropped_samples_delta(), 2);
        assert_eq!(rb.take_dropped_samples_delta(), 0);
    }

    #[test]
    fn test_dropped_samples_counts_full_block_drop_when_consumer_behind() {
        let mut rb = MonoRingBuffer::new(4);

        // Force an impossible state where the producer ran far ahead of the consumer.
        rb.read_pos = u32::MAX - 10;
        rb.write_pos = rb.read_pos.wrapping_add(8);

        let written = rb.write(&[1.0, 2.0, 3.0]);
        assert_eq!(written, 0);
        assert_eq!(rb.take_dropped_samples_delta(), 3);
        assert_eq!(rb.take_dropped_samples_delta(), 0);
    }

    #[test]
    fn test_read_updates_indices() {
        let mut rb = MonoRingBuffer::new(8);
        rb.write(&[0.0, 1.0, 2.0, 3.0]);

        let mut out = [0.0f32; 3];
        rb.read(&mut out);
        assert_eq!(rb.read_pos, 3);
        assert_eq!(rb.write_pos, 4);
    }
}

#[cfg(target_arch = "wasm32")]
mod wasm {
    use super::*;
    use js_sys::{Float32Array, SharedArrayBuffer, Uint32Array};
    use wasm_bindgen::prelude::*;

    #[wasm_bindgen]
    extern "C" {
        #[wasm_bindgen(js_namespace = Atomics)]
        fn load(array: &Uint32Array, index: u32) -> u32;

        #[wasm_bindgen(js_namespace = Atomics)]
        fn store(array: &Uint32Array, index: u32, value: u32) -> u32;
    }

    #[inline]
    fn atomic_load_u32(array: &Uint32Array, index: usize) -> u32 {
        load(array, index as u32)
    }

    #[inline]
    fn atomic_store_u32(array: &Uint32Array, index: usize, value: u32) {
        store(array, index as u32, value);
    }

    /// Consumer-side handle over the microphone capture ring buffer produced by
    /// the AudioWorklet.
    #[wasm_bindgen]
    pub struct MicBridge {
        capacity_samples: u32,
        header: Uint32Array,
        samples: Float32Array,
        last_dropped_samples: u32,
    }

    #[wasm_bindgen]
    impl MicBridge {
        /// Create a bridge over an existing microphone capture SharedArrayBuffer.
        ///
        /// Capacity is derived from the header (`CAPACITY_SAMPLES_INDEX`) if
        /// present, otherwise from the buffer size (for backwards
        /// compatibility).
        #[wasm_bindgen(js_name = fromSharedBuffer)]
        pub fn from_shared_buffer(sab: SharedArrayBuffer) -> Result<MicBridge, JsValue> {
            let byte_len = sab.byte_length() as usize;
            if byte_len < HEADER_BYTES {
                return Err(JsValue::from_str(
                    "SharedArrayBuffer is too small to contain a mic ring buffer header",
                ));
            }
            if !(byte_len - HEADER_BYTES).is_multiple_of(core::mem::size_of::<f32>()) {
                return Err(JsValue::from_str(
                    "SharedArrayBuffer mic ring buffer payload is not 4-byte aligned",
                ));
            }

            let payload_samples = ((byte_len - HEADER_BYTES) / core::mem::size_of::<f32>()) as u32;
            if payload_samples == 0 {
                return Err(JsValue::from_str(
                    "SharedArrayBuffer mic ring buffer contains no sample payload",
                ));
            }

            let header =
                Uint32Array::new_with_byte_offset_and_length(&sab, 0, HEADER_U32_LEN as u32);
            let capacity_from_header = atomic_load_u32(&header, CAPACITY_SAMPLES_INDEX);
            let capacity_samples = if capacity_from_header != 0 {
                if capacity_from_header != payload_samples {
                    return Err(JsValue::from_str(
                        "SharedArrayBuffer mic ring buffer capacity does not match buffer size",
                    ));
                }
                capacity_from_header
            } else {
                payload_samples
            };
            if capacity_samples > MAX_RING_CAPACITY_SAMPLES {
                return Err(JsValue::from_str(&format!(
                    "SharedArrayBuffer mic ring buffer capacity must be <= {MAX_RING_CAPACITY_SAMPLES}",
                )));
            }

            let samples = Float32Array::new_with_byte_offset_and_length(
                &sab,
                HEADER_BYTES as u32,
                capacity_samples,
            );

            let last_dropped_samples = atomic_load_u32(&header, DROPPED_SAMPLES_INDEX);

            Ok(Self {
                capacity_samples,
                header,
                samples,
                last_dropped_samples,
            })
        }

        pub fn buffered_samples(&self) -> u32 {
            let read_pos = atomic_load_u32(&self.header, READ_POS_INDEX);
            let write_pos = atomic_load_u32(&self.header, WRITE_POS_INDEX);
            samples_available_clamped(read_pos, write_pos, self.capacity_samples)
        }

        pub fn dropped_samples(&self) -> u32 {
            atomic_load_u32(&self.header, DROPPED_SAMPLES_INDEX)
        }

        /// Read up to `out.len()` samples from the ring buffer into `out`.
        ///
        /// Returns the number of samples read.
        pub fn read_f32_into(&self, out: &mut [f32]) -> u32 {
            if out.is_empty() {
                return 0;
            }

            let read_pos = atomic_load_u32(&self.header, READ_POS_INDEX);
            let write_pos = atomic_load_u32(&self.header, WRITE_POS_INDEX);
            let available = samples_available_clamped(read_pos, write_pos, self.capacity_samples);

            let requested = out.len() as u32;
            let to_read = requested.min(available);
            if to_read == 0 {
                return 0;
            }

            let start = read_pos % self.capacity_samples;
            let first_part = to_read.min(self.capacity_samples - start);
            let second_part = to_read - first_part;

            // Bulk copy using typed-array operations (at most 2 copies due to wrap-around).
            if first_part > 0 {
                let src = self.samples.subarray(start, start + first_part);
                src.copy_to(&mut out[..first_part as usize]);
            }
            if second_part > 0 {
                let src = self.samples.subarray(0, second_part);
                src.copy_to(&mut out[first_part as usize..(first_part + second_part) as usize]);
            }

            atomic_store_u32(&self.header, READ_POS_INDEX, read_pos.wrapping_add(to_read));

            to_read
        }
    }

    impl MicBridge {
        pub fn take_dropped_samples_delta(&mut self) -> u64 {
            let dropped = atomic_load_u32(&self.header, DROPPED_SAMPLES_INDEX);
            let delta = dropped.wrapping_sub(self.last_dropped_samples);
            self.last_dropped_samples = dropped;
            delta as u64
        }

        /// Discard any buffered microphone samples by advancing the consumer read position to the
        /// current producer write position.
        ///
        /// This is useful when resuming from a VM snapshot pause: the host-side microphone
        /// producer may have continued writing into the ring while the guest was stopped, so
        /// draining those samples would introduce stale capture latency.
        pub fn discard_buffered_samples(&mut self) {
            let write_pos = atomic_load_u32(&self.header, WRITE_POS_INDEX);
            atomic_store_u32(&self.header, READ_POS_INDEX, write_pos);
        }

        /// Reset the dropped-sample delta baseline to the current producer counter.
        ///
        /// The producer owns the `DROPPED_SAMPLES_INDEX` counter; this method does not mutate the
        /// shared counter, it only updates the consumer-side baseline used by
        /// [`Self::take_dropped_samples_delta`].
        pub fn reset_dropped_samples_baseline(&mut self) {
            self.last_dropped_samples = atomic_load_u32(&self.header, DROPPED_SAMPLES_INDEX);
        }
    }
}

#[cfg(target_arch = "wasm32")]
pub use wasm::MicBridge;
