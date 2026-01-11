use std::collections::VecDeque;

/// Audio capture source used by the HDA engine to pull mono microphone samples.
///
/// The samples are expected to be normalized `f32` in the `[-1.0, 1.0]` range.
pub trait AudioCaptureSource {
    /// Fill `out` with up to `out.len()` mono samples.
    ///
    /// Returns the number of samples written.
    fn read_mono_f32(&mut self, out: &mut [f32]) -> usize;

    /// Return the number of mono samples dropped by the capture backend since the last call.
    ///
    /// Most capture sources do not track dropped samples; the default implementation reports 0.
    fn take_dropped_samples(&mut self) -> u64 {
        0
    }
}

impl<T: AudioCaptureSource + ?Sized> AudioCaptureSource for Box<T> {
    fn read_mono_f32(&mut self, out: &mut [f32]) -> usize {
        (**self).read_mono_f32(out)
    }

    fn take_dropped_samples(&mut self) -> u64 {
        (**self).take_dropped_samples()
    }
}

/// Capture source that never produces samples (silence).
#[derive(Debug, Default, Clone, Copy)]
pub struct SilenceCaptureSource;

impl AudioCaptureSource for SilenceCaptureSource {
    fn read_mono_f32(&mut self, _out: &mut [f32]) -> usize {
        0
    }
}

/// Simple `VecDeque`-backed capture source for unit/integration tests.
#[derive(Debug, Default, Clone)]
pub struct VecDequeCaptureSource {
    samples: VecDeque<f32>,
}

impl VecDequeCaptureSource {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push_samples(&mut self, samples: &[f32]) {
        self.samples.extend(samples.iter().copied());
    }

    pub fn len(&self) -> usize {
        self.samples.len()
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }
}

impl AudioCaptureSource for VecDequeCaptureSource {
    fn read_mono_f32(&mut self, out: &mut [f32]) -> usize {
        let count = out.len().min(self.samples.len());
        for slot in out[..count].iter_mut() {
            *slot = self
                .samples
                .pop_front()
                .expect("VecDeque length checked above");
        }
        count
    }
}

impl AudioCaptureSource for aero_platform::audio::mic_bridge::MonoRingBuffer {
    fn read_mono_f32(&mut self, out: &mut [f32]) -> usize {
        self.read(out) as usize
    }

    fn take_dropped_samples(&mut self) -> u64 {
        self.take_dropped_samples_delta()
    }
}

#[cfg(target_arch = "wasm32")]
impl AudioCaptureSource for aero_platform::audio::mic_bridge::MicBridge {
    fn read_mono_f32(&mut self, out: &mut [f32]) -> usize {
        self.read_f32_into(out) as usize
    }

    fn take_dropped_samples(&mut self) -> u64 {
        self.take_dropped_samples_delta()
    }
}
