//! SharedArrayBuffer ring-buffer layout used to feed an `AudioWorkletProcessor`.
//!
//! The browser-side `AudioWorkletProcessor` is the consumer, and the emulator is
//! the producer. Indices are stored as monotonically increasing `u32` frame
//! counters (wrapping naturally at `2^32`) to avoid the classic "read == write"
//! ambiguity.

use aero_io_snapshot::io::audio::state::AudioWorkletRingState;

/// Header layout (`Uint32Array`) in the SharedArrayBuffer.
pub const HEADER_U32_LEN: usize = 4;

pub const READ_FRAME_INDEX: usize = 0;
pub const WRITE_FRAME_INDEX: usize = 1;
/// Total missing output frames rendered as silence due to underruns.
///
/// This is stored as a wrapping `u32` counter (wraps naturally at `2^32`).
pub const UNDERRUN_COUNT_INDEX: usize = 2;
/// Total frames dropped by the producer due to buffer full.
///
/// This is stored as a wrapping `u32` counter (wraps naturally at `2^32`).
pub const OVERRUN_COUNT_INDEX: usize = 3;

/// Total bytes reserved for the header.
pub const HEADER_BYTES: usize = HEADER_U32_LEN * 4;

/// Maximum ring capacity supported by the AudioWorklet ring-buffer helpers.
///
/// This module is used in several contexts:
/// - A pure-Rust [`InterleavedRingBuffer`] used by unit tests and native builds.
/// - A wasm [`WorkletBridge`] that owns a `SharedArrayBuffer` and typed-array views.
/// - Snapshot restore logic (`AudioWorkletRingState`) that may come from untrusted sources.
///
/// Capping the capacity prevents accidental multi-gigabyte allocations and bounds worst-case
/// restore behavior.
///
/// `2^20` frames is ~21s at 48kHz; at stereo f32 this is ~8MiB of sample storage.
const MAX_RING_CAPACITY_FRAMES: u32 = 1_048_576;

/// Maximum number of audio channels supported by the worklet ring buffer helpers.
///
/// The current Aero audio output contract is mono or stereo `f32` (interleaved). Bounding the
/// channel count prevents callers from accidentally allocating multi-gigabyte rings by passing an
/// absurd channel count.
const MAX_RING_CHANNEL_COUNT: u32 = 2;

#[inline]
pub fn frames_available(read_idx: u32, write_idx: u32) -> u32 {
    write_idx.wrapping_sub(read_idx)
}

#[inline]
pub fn frames_available_clamped(read_idx: u32, write_idx: u32, capacity_frames: u32) -> u32 {
    frames_available(read_idx, write_idx).min(capacity_frames)
}

#[inline]
pub fn frames_free(read_idx: u32, write_idx: u32, capacity_frames: u32) -> u32 {
    capacity_frames - frames_available_clamped(read_idx, write_idx, capacity_frames)
}

#[inline]
pub fn buffer_byte_len(capacity_frames: u32, channel_count: u32) -> usize {
    HEADER_BYTES + capacity_frames as usize * channel_count as usize * core::mem::size_of::<f32>()
}

#[cfg(target_arch = "wasm32")]
#[inline]
fn layout_bytes_u32(capacity_frames: u32, channel_count: u32) -> Option<(u32, u32)> {
    let sample_capacity = capacity_frames.checked_mul(channel_count)?;
    let sample_bytes = sample_capacity.checked_mul(core::mem::size_of::<f32>() as u32)?;
    let byte_len = (HEADER_BYTES as u32).checked_add(sample_bytes)?;
    Some((byte_len, sample_capacity))
}

/// A small, pure-Rust ring buffer used for unit testing index math and wrap-around.
///
/// In wasm, the actual storage is a `SharedArrayBuffer` accessed via typed-array
/// views on both the producer and consumer side.
#[derive(Debug)]
pub struct InterleavedRingBuffer {
    capacity_frames: u32,
    channel_count: u32,
    read_idx: u32,
    write_idx: u32,
    storage: Vec<f32>,
}

impl InterleavedRingBuffer {
    pub fn new(capacity_frames: u32, channel_count: u32) -> Self {
        assert!(capacity_frames > 0, "capacity_frames must be non-zero");
        assert!(channel_count > 0, "channel_count must be non-zero");
        let capacity_frames = capacity_frames.min(MAX_RING_CAPACITY_FRAMES);
        let channel_count = channel_count.min(MAX_RING_CHANNEL_COUNT);
        Self {
            capacity_frames,
            channel_count,
            read_idx: 0,
            write_idx: 0,
            storage: vec![0.0; capacity_frames as usize * channel_count as usize],
        }
    }

    pub fn buffer_level_frames(&self) -> u32 {
        frames_available_clamped(self.read_idx, self.write_idx, self.capacity_frames)
    }

    pub fn snapshot_state(&self) -> AudioWorkletRingState {
        AudioWorkletRingState {
            capacity_frames: self.capacity_frames,
            write_pos: self.write_idx,
            read_pos: self.read_idx,
        }
    }

    /// Restore ring buffer indices from snapshot state.
    ///
    /// The ring's sample contents are not restored; storage is cleared to silence.
    pub fn restore_state(&mut self, state: &AudioWorkletRingState) {
        if state.capacity_frames != 0 {
            let restored_capacity = state.capacity_frames.clamp(1, MAX_RING_CAPACITY_FRAMES);
            if restored_capacity != self.capacity_frames {
                self.capacity_frames = restored_capacity;
                let samples = self.capacity_frames.saturating_mul(self.channel_count) as usize;
                self.storage.resize(samples, 0.0);
            }
        }
        self.storage.fill(0.0);

        let mut read = state.read_pos;
        let write = state.write_pos;
        let available = frames_available(read, write);
        if available > self.capacity_frames {
            // Producer is ahead by more than the ring can hold. Clamp to a consistent "full"
            // state so reads/writes can make progress immediately (rather than taking billions of
            // frames to drain due to wrapping arithmetic).
            read = write.wrapping_sub(self.capacity_frames);
        }

        self.read_idx = read;
        self.write_idx = write;
    }

    pub fn write_interleaved(&mut self, samples: &[f32]) -> u32 {
        let requested_frames = (samples.len() as u32) / self.channel_count;
        if requested_frames == 0 {
            return 0;
        }

        let free = frames_free(self.read_idx, self.write_idx, self.capacity_frames);
        let frames_to_write = requested_frames.min(free);
        if frames_to_write == 0 {
            return 0;
        }

        let write_pos = self.write_idx % self.capacity_frames;
        let first_frames = frames_to_write.min(self.capacity_frames - write_pos);
        let second_frames = frames_to_write - first_frames;

        let cc = self.channel_count as usize;
        let first_samples = first_frames as usize * cc;
        let write_sample_pos = write_pos as usize * cc;
        self.storage[write_sample_pos..write_sample_pos + first_samples]
            .copy_from_slice(&samples[..first_samples]);

        if second_frames > 0 {
            let second_samples = second_frames as usize * cc;
            self.storage[..second_samples]
                .copy_from_slice(&samples[first_samples..first_samples + second_samples]);
        }

        self.write_idx = self.write_idx.wrapping_add(frames_to_write);
        frames_to_write
    }

    pub fn read_interleaved(&mut self, out: &mut [f32]) -> u32 {
        let requested_frames = (out.len() as u32) / self.channel_count;
        if requested_frames == 0 {
            return 0;
        }

        let available =
            frames_available_clamped(self.read_idx, self.write_idx, self.capacity_frames);
        let frames_to_read = requested_frames.min(available);
        if frames_to_read == 0 {
            return 0;
        }

        let read_pos = self.read_idx % self.capacity_frames;
        let first_frames = frames_to_read.min(self.capacity_frames - read_pos);
        let second_frames = frames_to_read - first_frames;

        let cc = self.channel_count as usize;
        let first_samples = first_frames as usize * cc;
        let read_sample_pos = read_pos as usize * cc;
        out[..first_samples]
            .copy_from_slice(&self.storage[read_sample_pos..read_sample_pos + first_samples]);

        if second_frames > 0 {
            let second_samples = second_frames as usize * cc;
            out[first_samples..first_samples + second_samples]
                .copy_from_slice(&self.storage[..second_samples]);
        }

        self.read_idx = self.read_idx.wrapping_add(frames_to_read);
        frames_to_read
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_frames_available_wraps_u32() {
        let read = u32::MAX - 10;
        let write = read.wrapping_add(5);
        assert_eq!(frames_available(read, write), 5);
    }

    #[test]
    fn test_ring_buffer_wrap_around_preserves_order() {
        let mut rb = InterleavedRingBuffer::new(4, 2);

        // Write 3 frames: [0,0], [1,1], [2,2]
        let written = rb.write_interleaved(&[0.0, 0.0, 1.0, 1.0, 2.0, 2.0]);
        assert_eq!(written, 3);
        assert_eq!(rb.buffer_level_frames(), 3);

        // Read 2 frames, should get 0 and 1.
        let mut out = [0.0f32; 4];
        let read = rb.read_interleaved(&mut out);
        assert_eq!(read, 2);
        assert_eq!(out, [0.0, 0.0, 1.0, 1.0]);
        assert_eq!(rb.buffer_level_frames(), 1);

        // Now write 3 more frames; only 3 free frames are available.
        let written = rb.write_interleaved(&[3.0, 3.0, 4.0, 4.0, 5.0, 5.0]);
        assert_eq!(written, 3);
        assert_eq!(rb.buffer_level_frames(), 4);

        // Read 4 frames; should see remaining 2, then 3,4,5 in order.
        let mut out = [0.0f32; 8];
        let read = rb.read_interleaved(&mut out);
        assert_eq!(read, 4);
        assert_eq!(out, [2.0, 2.0, 3.0, 3.0, 4.0, 4.0, 5.0, 5.0]);
        assert_eq!(rb.buffer_level_frames(), 0);
    }

    #[test]
    fn test_snapshot_restore_preserves_indices_and_capacity() {
        let mut rb = InterleavedRingBuffer::new(8, 2);
        let written = rb.write_interleaved(&[1.0f32; 6 * 2]);
        assert_eq!(written, 6);

        let mut out = vec![0.0f32; 2 * 2];
        let read = rb.read_interleaved(&mut out);
        assert_eq!(read, 2);

        let state = rb.snapshot_state();
        assert_eq!(
            state,
            AudioWorkletRingState {
                capacity_frames: 8,
                write_pos: 6,
                read_pos: 2,
            }
        );

        // Restore into a ring with a different capacity; restore should resize to match.
        let mut restored = InterleavedRingBuffer::new(4, 2);
        restored.restore_state(&state);
        assert_eq!(restored.snapshot_state(), state);
        assert_eq!(restored.buffer_level_frames(), 4);

        // Verify that subsequent writes see the same free space as the original at snapshot time.
        let write_req = vec![2.0f32; 10 * 2];
        assert_eq!(restored.write_interleaved(&write_req), 4);
        assert_eq!(restored.buffer_level_frames(), 8);
    }

    #[test]
    fn test_snapshot_restore_handles_wrapping_indices() {
        let state = AudioWorkletRingState {
            capacity_frames: 8,
            read_pos: u32::MAX - 2,
            write_pos: (u32::MAX - 2).wrapping_add(5),
        };

        let mut rb = InterleavedRingBuffer::new(8, 2);
        rb.restore_state(&state);
        assert_eq!(rb.buffer_level_frames(), 5);
        assert_eq!(rb.snapshot_state(), state);
    }

    #[test]
    fn test_snapshot_restore_clamps_excessive_capacity() {
        let state = AudioWorkletRingState {
            capacity_frames: u32::MAX,
            read_pos: 0,
            write_pos: 0,
        };
        let mut rb = InterleavedRingBuffer::new(8, 2);
        rb.restore_state(&state);
        assert_eq!(
            rb.snapshot_state().capacity_frames,
            MAX_RING_CAPACITY_FRAMES
        );
    }

    #[test]
    fn test_new_clamps_excessive_capacity_to_avoid_oom() {
        let rb = InterleavedRingBuffer::new(u32::MAX, 2);
        assert_eq!(
            rb.snapshot_state().capacity_frames,
            MAX_RING_CAPACITY_FRAMES
        );
    }

    #[test]
    fn test_new_clamps_excessive_channel_count_to_avoid_oom() {
        let rb = InterleavedRingBuffer::new(8, u32::MAX);
        assert_eq!(rb.channel_count, MAX_RING_CHANNEL_COUNT);
        assert_eq!(
            rb.storage.len(),
            rb.capacity_frames as usize * rb.channel_count as usize
        );
    }

    #[test]
    fn test_snapshot_restore_clamps_indices_when_write_ahead_of_capacity() {
        let state = AudioWorkletRingState {
            capacity_frames: 8,
            read_pos: 0,
            write_pos: 100,
        };
        let mut rb = InterleavedRingBuffer::new(8, 2);
        rb.restore_state(&state);
        assert_eq!(rb.buffer_level_frames(), 8);

        // After reading part of the buffer, the level should decrease. Without index clamping,
        // the ring would appear permanently full because `write_pos - read_pos` would remain
        // far larger than `capacity_frames`.
        let mut out = vec![0.0f32; 4 * 2];
        let read = rb.read_interleaved(&mut out);
        assert_eq!(read, 4);
        assert_eq!(rb.buffer_level_frames(), 4);
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

        #[wasm_bindgen(js_namespace = Atomics)]
        fn add(array: &Uint32Array, index: u32, value: u32) -> u32;
    }

    #[inline]
    fn atomic_load_u32(array: &Uint32Array, index: usize) -> u32 {
        load(array, index as u32)
    }

    #[inline]
    fn atomic_store_u32(array: &Uint32Array, index: usize, value: u32) {
        store(array, index as u32, value);
    }

    #[inline]
    fn atomic_add_u32(array: &Uint32Array, index: usize, value: u32) {
        let _ = add(array, index as u32, value);
    }

    /// Producer-side handle to the SharedArrayBuffer ring buffer consumed by the
    /// AudioWorklet.
    #[wasm_bindgen]
    pub struct WorkletBridge {
        sab: SharedArrayBuffer,
        capacity_frames: u32,
        channel_count: u32,
        header: Uint32Array,
        samples: Float32Array,
    }

    #[wasm_bindgen]
    impl WorkletBridge {
        /// Allocate a new SharedArrayBuffer ring buffer.
        #[wasm_bindgen(constructor)]
        pub fn new(capacity_frames: u32, channel_count: u32) -> Result<WorkletBridge, JsValue> {
            if capacity_frames == 0 {
                return Err(JsValue::from_str("capacity_frames must be non-zero"));
            }
            if channel_count == 0 {
                return Err(JsValue::from_str("channel_count must be non-zero"));
            }
            if channel_count > MAX_RING_CHANNEL_COUNT {
                return Err(JsValue::from_str(&format!(
                    "channel_count must be <= {MAX_RING_CHANNEL_COUNT}"
                )));
            }

            let (byte_len, sample_capacity) = layout_bytes_u32(capacity_frames, channel_count)
                .ok_or_else(|| JsValue::from_str("Requested ring buffer layout exceeds 4GiB"))?;
            if capacity_frames > MAX_RING_CAPACITY_FRAMES {
                return Err(JsValue::from_str(&format!(
                    "capacity_frames must be <= {MAX_RING_CAPACITY_FRAMES}"
                )));
            }
            let sab = SharedArrayBuffer::new(byte_len);

            let header =
                Uint32Array::new_with_byte_offset_and_length(&sab, 0, HEADER_U32_LEN as u32);
            let samples = Float32Array::new_with_byte_offset_and_length(
                &sab,
                HEADER_BYTES as u32,
                sample_capacity,
            );

            // Explicitly reset shared state.
            atomic_store_u32(&header, READ_FRAME_INDEX, 0);
            atomic_store_u32(&header, WRITE_FRAME_INDEX, 0);
            atomic_store_u32(&header, UNDERRUN_COUNT_INDEX, 0);
            atomic_store_u32(&header, OVERRUN_COUNT_INDEX, 0);

            Ok(Self {
                sab,
                capacity_frames,
                channel_count,
                header,
                samples,
            })
        }

        /// Create a bridge over an existing SharedArrayBuffer ring buffer.
        ///
        /// This is useful when the JS side allocates the SharedArrayBuffer (e.g.
        /// as part of UI initialization) and the WASM side needs to treat it as
        /// the producer ring buffer.
        #[wasm_bindgen(js_name = fromSharedBuffer)]
        pub fn from_shared_buffer(
            sab: SharedArrayBuffer,
            capacity_frames: u32,
            channel_count: u32,
        ) -> Result<WorkletBridge, JsValue> {
            if capacity_frames == 0 {
                return Err(JsValue::from_str("capacity_frames must be non-zero"));
            }
            if channel_count == 0 {
                return Err(JsValue::from_str("channel_count must be non-zero"));
            }
            if channel_count > MAX_RING_CHANNEL_COUNT {
                return Err(JsValue::from_str(&format!(
                    "channel_count must be <= {MAX_RING_CHANNEL_COUNT}"
                )));
            }

            let (required, sample_capacity) = layout_bytes_u32(capacity_frames, channel_count)
                .ok_or_else(|| JsValue::from_str("Requested ring buffer layout exceeds 4GiB"))?;
            if capacity_frames > MAX_RING_CAPACITY_FRAMES {
                return Err(JsValue::from_str(&format!(
                    "capacity_frames must be <= {MAX_RING_CAPACITY_FRAMES}"
                )));
            }
            if sab.byte_length() < required {
                return Err(JsValue::from_str(
                    "SharedArrayBuffer is too small for the requested layout",
                ));
            }

            let header =
                Uint32Array::new_with_byte_offset_and_length(&sab, 0, HEADER_U32_LEN as u32);
            let samples = Float32Array::new_with_byte_offset_and_length(
                &sab,
                HEADER_BYTES as u32,
                sample_capacity,
            );

            Ok(Self {
                sab,
                capacity_frames,
                channel_count,
                header,
                samples,
            })
        }

        #[wasm_bindgen(getter)]
        pub fn shared_buffer(&self) -> SharedArrayBuffer {
            self.sab.clone()
        }

        #[wasm_bindgen(getter)]
        pub fn capacity_frames(&self) -> u32 {
            self.capacity_frames
        }

        #[wasm_bindgen(getter)]
        pub fn channel_count(&self) -> u32 {
            self.channel_count
        }

        /// Write interleaved `f32` frames into the ring buffer.
        ///
        /// Returns the number of frames written. If the buffer is full, this
        /// returns 0 without blocking.
        pub fn write_f32_interleaved(&self, samples: &[f32]) -> u32 {
            let requested_frames = (samples.len() as u32) / self.channel_count;
            if requested_frames == 0 {
                return 0;
            }

            let read_idx = atomic_load_u32(&self.header, READ_FRAME_INDEX);
            let write_idx = atomic_load_u32(&self.header, WRITE_FRAME_INDEX);

            let free = frames_free(read_idx, write_idx, self.capacity_frames);
            let frames_to_write = requested_frames.min(free);
            if frames_to_write < requested_frames {
                atomic_add_u32(
                    &self.header,
                    OVERRUN_COUNT_INDEX,
                    requested_frames - frames_to_write,
                );
            }
            if frames_to_write == 0 {
                return 0;
            }

            let write_pos = write_idx % self.capacity_frames;
            let first_frames = frames_to_write.min(self.capacity_frames - write_pos);
            let second_frames = frames_to_write - first_frames;

            let cc = self.channel_count as usize;
            let first_samples = first_frames as usize * cc;
            let write_sample_pos = write_pos * self.channel_count;
            self.samples
                .subarray(write_sample_pos, write_sample_pos + first_samples as u32)
                .copy_from(&samples[..first_samples]);

            if second_frames > 0 {
                let second_samples = second_frames as usize * cc;
                self.samples
                    .subarray(0, second_samples as u32)
                    .copy_from(&samples[first_samples..first_samples + second_samples]);
            }

            atomic_store_u32(
                &self.header,
                WRITE_FRAME_INDEX,
                write_idx.wrapping_add(frames_to_write),
            );
            frames_to_write
        }

        pub fn buffer_level_frames(&self) -> u32 {
            let read_idx = atomic_load_u32(&self.header, READ_FRAME_INDEX);
            let write_idx = atomic_load_u32(&self.header, WRITE_FRAME_INDEX);
            frames_available_clamped(read_idx, write_idx, self.capacity_frames)
        }

        /// Total missing output frames rendered as silence due to underruns.
        ///
        /// This is a wrapping `u32` counter (wraps naturally at `2^32`).
        pub fn underrun_count(&self) -> u32 {
            atomic_load_u32(&self.header, UNDERRUN_COUNT_INDEX)
        }

        /// Total frames dropped by the producer due to buffer full.
        ///
        /// This is a wrapping `u32` counter (wraps naturally at `2^32`).
        pub fn overrun_count(&self) -> u32 {
            atomic_load_u32(&self.header, OVERRUN_COUNT_INDEX)
        }
    }

    impl WorkletBridge {
        pub fn snapshot_state(&self) -> AudioWorkletRingState {
            AudioWorkletRingState {
                capacity_frames: self.capacity_frames,
                write_pos: atomic_load_u32(&self.header, WRITE_FRAME_INDEX),
                read_pos: atomic_load_u32(&self.header, READ_FRAME_INDEX),
            }
        }

        /// Restore ring indices from snapshot state.
        ///
        /// This does not restore the ring's sample contents; any previously-buffered host audio is
        /// dropped and replaced with silence to avoid replaying stale samples after restore.
        ///
        /// Note: underrun/overrun counters are host-side telemetry and are not part of the
        /// snapshot; restore intentionally leaves them untouched.
        pub fn restore_state(&self, state: &AudioWorkletRingState) {
            // Snapshot restore should generally recreate the same ring capacity (the host runtime
            // decides the ring size). However, for robustness (and because snapshots may come from
            // untrusted/corrupt sources), tolerate mismatches safely by clamping against the
            // smaller of the snapshot capacity and the actual ring capacity.
            //
            // This avoids restoring a state whose `write_pos - read_pos` would imply an impossible
            // buffer level for the current ring and prevents pathological "permanently full" rings
            // when restoring from untrusted/corrupt snapshot inputs.
            let effective_capacity = if state.capacity_frames != 0 {
                state.capacity_frames.min(self.capacity_frames)
            } else {
                self.capacity_frames
            };

            // Clear sample contents to silence. The snapshot only preserves the indices (for
            // determinism), not the audio content itself.
            let _ = self.samples.fill(0.0, 0, self.samples.length());

            let mut read = state.read_pos;
            let write = state.write_pos;
            let available = frames_available(read, write);
            if available > effective_capacity {
                read = write.wrapping_sub(effective_capacity);
            }

            atomic_store_u32(&self.header, READ_FRAME_INDEX, read);
            atomic_store_u32(&self.header, WRITE_FRAME_INDEX, write);
        }
    }
}

#[cfg(target_arch = "wasm32")]
pub use wasm::WorkletBridge;
