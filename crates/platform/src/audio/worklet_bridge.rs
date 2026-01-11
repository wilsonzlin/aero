//! SharedArrayBuffer ring-buffer layout used to feed an `AudioWorkletProcessor`.
//!
//! The browser-side `AudioWorkletProcessor` is the consumer, and the emulator is
//! the producer. Indices are stored as monotonically increasing `u32` frame
//! counters (wrapping naturally at `2^32`) to avoid the classic "read == write"
//! ambiguity.

/// Header layout (`Uint32Array`) in the SharedArrayBuffer.
pub const HEADER_U32_LEN: usize = 4;

pub const READ_FRAME_INDEX: usize = 0;
pub const WRITE_FRAME_INDEX: usize = 1;
pub const UNDERRUN_COUNT_INDEX: usize = 2;
pub const OVERRUN_COUNT_INDEX: usize = 3;

/// Total bytes reserved for the header.
pub const HEADER_BYTES: usize = HEADER_U32_LEN * 4;

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

        let available = frames_available_clamped(self.read_idx, self.write_idx, self.capacity_frames);
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
            out[first_samples..first_samples + second_samples].copy_from_slice(&self.storage[..second_samples]);
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

            let byte_len = buffer_byte_len(capacity_frames, channel_count) as u32;
            let sab = SharedArrayBuffer::new(byte_len);

            let header = Uint32Array::new_with_byte_offset_and_length(&sab, 0, HEADER_U32_LEN as u32);
            let samples = Float32Array::new_with_byte_offset_and_length(
                &sab,
                HEADER_BYTES as u32,
                capacity_frames * channel_count,
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

            let required = buffer_byte_len(capacity_frames, channel_count) as u32;
            if sab.byte_length() < required {
                return Err(JsValue::from_str("SharedArrayBuffer is too small for the requested layout"));
            }

            let header = Uint32Array::new_with_byte_offset_and_length(&sab, 0, HEADER_U32_LEN as u32);
            let samples = Float32Array::new_with_byte_offset_and_length(
                &sab,
                HEADER_BYTES as u32,
                capacity_frames * channel_count,
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
            let write_sample_pos = write_pos as u32 * self.channel_count;
            self.samples
                .subarray(write_sample_pos, write_sample_pos + first_samples as u32)
                .copy_from(&samples[..first_samples]);

            if second_frames > 0 {
                let second_samples = second_frames as usize * cc;
                self.samples
                    .subarray(0, second_samples as u32)
                    .copy_from(&samples[first_samples..first_samples + second_samples]);
            }

            atomic_store_u32(&self.header, WRITE_FRAME_INDEX, write_idx.wrapping_add(frames_to_write));
            frames_to_write
        }

        pub fn buffer_level_frames(&self) -> u32 {
            let read_idx = atomic_load_u32(&self.header, READ_FRAME_INDEX);
            let write_idx = atomic_load_u32(&self.header, WRITE_FRAME_INDEX);
            frames_available_clamped(read_idx, write_idx, self.capacity_frames)
        }

        pub fn underrun_count(&self) -> u32 {
            atomic_load_u32(&self.header, UNDERRUN_COUNT_INDEX)
        }

        pub fn overrun_count(&self) -> u32 {
            atomic_load_u32(&self.header, OVERRUN_COUNT_INDEX)
        }
    }
}

#[cfg(target_arch = "wasm32")]
pub use wasm::WorkletBridge;
