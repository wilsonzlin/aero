//! Audio input plumbing (microphone capture → guest-visible capture stream).
//!
//! The browser side captures `f32` PCM frames via Web Audio and writes them to a
//! ring buffer. The emulator consumes these samples and converts them into the
//! guest device's preferred format (typically 16-bit PCM).

use std::cmp;

use super::dsp;

/// A single-producer/single-consumer ring buffer for `f32` PCM samples.
///
/// This is intended as the "bridge buffer" between the browser microphone
/// capture pipeline and the emulated audio device model.
///
/// Design goals:
/// - Bounded memory usage (fixed capacity).
/// - Favor low-latency capture: when the consumer is too slow, we drop the
///   oldest part of the current write block (keeping the most recent samples)
///   instead of growing latency unbounded.
#[derive(Debug)]
pub struct F32RingBuffer {
    buf: Vec<f32>,
    cap: u32,

    // Monotonic cursors (wrapping at 2^32). Stored as u32 because it mirrors the
    // indices we use in SharedArrayBuffer on the JS side.
    write_pos: u32,
    read_pos: u32,

    dropped_samples: u64,
}

impl F32RingBuffer {
    /// Create a new ring buffer with the given capacity in samples.
    ///
    /// `capacity_samples` must be non-zero.
    pub fn new(capacity_samples: usize) -> Self {
        assert!(capacity_samples > 0, "capacity must be non-zero");
        let cap = capacity_samples as u32;
        Self {
            buf: vec![0.0; capacity_samples],
            cap,
            write_pos: 0,
            read_pos: 0,
            dropped_samples: 0,
        }
    }

    /// Number of samples currently stored and available for reading.
    pub fn available(&self) -> usize {
        // We always keep the invariant `write_pos - read_pos <= cap` by
        // dropping writes when full, so wrapping_sub is safe.
        self.write_pos.wrapping_sub(self.read_pos) as usize
    }

    /// Total number of samples dropped due to buffer pressure.
    pub fn dropped_samples(&self) -> u64 {
        self.dropped_samples
    }

    /// Maximum number of samples the buffer can hold.
    pub fn capacity(&self) -> usize {
        self.cap as usize
    }

    /// Clear the buffer contents (resets cursors and drop counter).
    pub fn reset(&mut self) {
        self.write_pos = 0;
        self.read_pos = 0;
        self.dropped_samples = 0;
        self.buf.fill(0.0);
    }

    /// Write as many samples as possible into the ring buffer.
    ///
    /// Returns the number of samples written. If the buffer does not have
    /// enough free space, the remaining samples are dropped and counted.
    pub fn write(&mut self, samples: &[f32]) -> usize {
        let used = self.available() as u32;
        let free = self.cap - used;
        let to_write = cmp::min(samples.len() as u32, free) as usize;
        let dropped = samples.len() - to_write;

        // Bias towards low latency by keeping the most recent part of a block
        // when we have to drop.
        let samples = &samples[dropped..];

        for i in 0..to_write {
            let idx = self.write_pos.wrapping_add(i as u32) % self.cap;
            self.buf[idx as usize] = samples[i];
        }

        self.write_pos = self.write_pos.wrapping_add(to_write as u32);

        if dropped != 0 {
            self.dropped_samples += dropped as u64;
        }

        to_write
    }

    /// Read up to `out.len()` samples from the ring buffer into `out`.
    ///
    /// Returns the number of samples actually read.
    pub fn read(&mut self, out: &mut [f32]) -> usize {
        let available = self.available() as u32;
        let to_read = cmp::min(out.len() as u32, available) as usize;

        for i in 0..to_read {
            let idx = self.read_pos.wrapping_add(i as u32) % self.cap;
            out[i] = self.buf[idx as usize];
        }

        self.read_pos = self.read_pos.wrapping_add(to_read as u32);
        to_read
    }

    /// Convenience helper: read `f32` samples and convert them into 16-bit PCM.
    ///
    /// To avoid allocations in the hot path, callers provide a temporary `f32`
    /// buffer (`tmp_f32`) that must be at least as large as `out_pcm16`.
    ///
    /// Returns the number of samples written into `out_pcm16`.
    pub fn read_pcm16(&mut self, tmp_f32: &mut [f32], out_pcm16: &mut [i16]) -> usize {
        assert!(
            tmp_f32.len() >= out_pcm16.len(),
            "tmp_f32 must be at least as large as out_pcm16"
        );

        let n = self.read(&mut tmp_f32[..out_pcm16.len()]);
        dsp::convert_f32_to_i16(&tmp_f32[..n], &mut out_pcm16[..n]);
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_buffer_roundtrip() {
        let mut rb = F32RingBuffer::new(4);

        assert_eq!(rb.available(), 0);
        assert_eq!(rb.write(&[0.1, 0.2, 0.3]), 3);
        assert_eq!(rb.available(), 3);

        let mut out = [0.0; 2];
        assert_eq!(rb.read(&mut out), 2);
        assert_eq!(out, [0.1, 0.2]);
        assert_eq!(rb.available(), 1);

        let mut out2 = [0.0; 2];
        assert_eq!(rb.read(&mut out2), 1);
        assert_eq!(out2[0], 0.3);
        assert_eq!(rb.available(), 0);
    }

    #[test]
    fn ring_buffer_wraparound() {
        let mut rb = F32RingBuffer::new(4);

        assert_eq!(rb.write(&[1.0, 2.0, 3.0, 4.0]), 4);
        let mut out = [0.0; 3];
        assert_eq!(rb.read(&mut out), 3);
        assert_eq!(out, [1.0, 2.0, 3.0]);

        // Now write should wrap.
        assert_eq!(rb.write(&[5.0, 6.0, 7.0]), 3);
        assert_eq!(rb.available(), 4);

        let mut out2 = [0.0; 4];
        assert_eq!(rb.read(&mut out2), 4);
        assert_eq!(out2, [4.0, 5.0, 6.0, 7.0]);
    }

    #[test]
    fn ring_buffer_drops_when_full() {
        let mut rb = F32RingBuffer::new(4);

        assert_eq!(rb.write(&[1.0, 2.0, 3.0, 4.0]), 4);
        assert_eq!(rb.write(&[5.0, 6.0]), 0);
        assert_eq!(rb.dropped_samples(), 2);

        let mut out = [0.0; 4];
        assert_eq!(rb.read(&mut out), 4);
        assert_eq!(out, [1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn ring_buffer_keeps_most_recent_on_partial_drop() {
        let mut rb = F32RingBuffer::new(4);

        assert_eq!(rb.write(&[10.0, 11.0, 12.0]), 3);
        // Only 1 slot free; we should keep the most recent sample (22.0).
        assert_eq!(rb.write(&[20.0, 21.0, 22.0]), 1);
        assert_eq!(rb.dropped_samples(), 2);

        let mut out = [0.0; 4];
        assert_eq!(rb.read(&mut out), 4);
        assert_eq!(out, [10.0, 11.0, 12.0, 22.0]);
    }

    #[test]
    fn ring_buffer_pcm16_conversion() {
        let mut rb = F32RingBuffer::new(8);
        rb.write(&[-1.0, -0.5, 0.0, 0.5, 1.0]);

        let mut tmp = [0.0f32; 5];
        let mut out = [0i16; 5];
        let n = rb.read_pcm16(&mut tmp, &mut out);
        assert_eq!(n, 5);
        assert_eq!(out[0], -32768);
        assert_eq!(out[2], 0);
        assert_eq!(out[4], 32767);
    }
}
