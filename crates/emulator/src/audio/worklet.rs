use aero_platform::audio::worklet_bridge::InterleavedRingBuffer;
use core::ops::{Deref, DerefMut};

/// Stereo output is the only format currently exposed by the virtio-snd device.
pub const AUDIO_CHANNELS: u32 = 2;

/// A small, pure-Rust ring buffer compatible with the AU-WORKLET SharedArrayBuffer layout.
///
/// This wraps `aero_platform::audio::worklet_bridge::InterleavedRingBuffer` while fixing the
/// channel count to stereo.
#[derive(Debug)]
pub struct AudioWorkletRingBuffer {
    inner: InterleavedRingBuffer,
}

impl AudioWorkletRingBuffer {
    /// Create a stereo ring buffer with the requested capacity in frames.
    pub fn new(capacity_frames: u32) -> Self {
        Self {
            inner: InterleavedRingBuffer::new(capacity_frames, AUDIO_CHANNELS),
        }
    }

    pub fn buffer_level_frames(&self) -> u32 {
        self.inner.buffer_level_frames()
    }
}

impl Deref for AudioWorkletRingBuffer {
    type Target = InterleavedRingBuffer;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl DerefMut for AudioWorkletRingBuffer {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

/// Sink for interleaved stereo `f32` audio frames.
pub trait AudioSink {
    /// Push interleaved stereo samples (L0, R0, L1, R1, ...) and return the number of frames
    /// written.
    fn push_stereo_f32(&mut self, samples: &[f32]) -> u32;
}

impl AudioSink for AudioWorkletRingBuffer {
    fn push_stereo_f32(&mut self, samples: &[f32]) -> u32 {
        self.inner.write_interleaved(samples)
    }
}

#[cfg(target_arch = "wasm32")]
impl AudioSink for aero_platform::audio::worklet_bridge::WorkletBridge {
    fn push_stereo_f32(&mut self, samples: &[f32]) -> u32 {
        self.write_f32_interleaved(samples)
    }
}

#[cfg(test)]
mod tests {
    use super::AudioWorkletRingBuffer;

    #[test]
    fn ring_buffer_wraps_and_tracks_frames() {
        let mut rb = AudioWorkletRingBuffer::new(4);
        assert_eq!(
            rb.write_interleaved(&[1.0, 1.0, 2.0, 2.0, 3.0, 3.0]),
            3
        );
        assert_eq!(rb.buffer_level_frames(), 3);

        let mut out = [0.0; 4];
        assert_eq!(rb.read_interleaved(&mut out), 2);
        assert_eq!(out, [1.0, 1.0, 2.0, 2.0]);
        assert_eq!(rb.buffer_level_frames(), 1);

        assert_eq!(
            rb.write_interleaved(&[4.0, 4.0, 5.0, 5.0, 6.0, 6.0]),
            3
        );
        assert_eq!(rb.buffer_level_frames(), 4);

        let mut out = [0.0; 8];
        assert_eq!(rb.read_interleaved(&mut out), 4);
        assert_eq!(out, [3.0, 3.0, 4.0, 4.0, 5.0, 5.0, 6.0, 6.0]);
        assert_eq!(rb.buffer_level_frames(), 0);
    }
}

