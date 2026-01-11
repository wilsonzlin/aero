/// Audio sink used by the HDA engine to emit interleaved `f32` samples.
///
/// This is intentionally small to make it easy to bridge the device model to
/// different backends:
/// - an in-memory ring buffer (unit tests)
/// - a `SharedArrayBuffer` ring buffer consumed by an `AudioWorkletProcessor`
pub trait AudioSink {
    /// Push interleaved `f32` samples.
    ///
    /// Samples are expected to be interleaved by channel (e.g. stereo: L0, R0, L1, R1, ...).
    fn push_interleaved_f32(&mut self, samples: &[f32]);
}

impl AudioSink for crate::ring::AudioRingBuffer {
    fn push_interleaved_f32(&mut self, samples: &[f32]) {
        self.push_interleaved_stereo(samples);
    }
}

impl AudioSink for aero_platform::audio::worklet_bridge::InterleavedRingBuffer {
    fn push_interleaved_f32(&mut self, samples: &[f32]) {
        let _ = self.write_interleaved(samples);
    }
}

#[cfg(target_arch = "wasm32")]
impl AudioSink for aero_platform::audio::worklet_bridge::WorkletBridge {
    fn push_interleaved_f32(&mut self, samples: &[f32]) {
        let _ = self.write_f32_interleaved(samples);
    }
}
