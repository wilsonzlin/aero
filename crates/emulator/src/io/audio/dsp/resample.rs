use core::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResampleError {
    InvalidChannels,
    InvalidRates,
    InputLengthNotAligned { expected_multiple: usize },
}

impl fmt::Display for ResampleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidChannels => write!(f, "invalid channel count"),
            Self::InvalidRates => write!(f, "invalid sample rate"),
            Self::InputLengthNotAligned { expected_multiple } => write!(
                f,
                "resample input length is not a multiple of {expected_multiple} samples"
            ),
        }
    }
}

impl std::error::Error for ResampleError {}

/// Streaming linear resampler for interleaved f32 audio.
///
/// This resampler is stateful to preserve phase between calls. It requires at least one
/// lookahead frame, so it may hold back the last input frame until the next call.
pub struct LinearResampler {
    src_rate: u32,
    dst_rate: u32,
    channels: usize,

    // Position in the current "extended" input buffer:
    // [prev_frame] + current_input_frames
    //
    // `pos = 1.0` corresponds to the first sample of `current_input_frames` (time t = 0).
    pos: f64,
    step: f64,

    prev_frame: Vec<f32>,
    have_prev: bool,
}

impl LinearResampler {
    pub fn new(src_rate: u32, dst_rate: u32, channels: usize) -> Result<Self, ResampleError> {
        if channels == 0 {
            return Err(ResampleError::InvalidChannels);
        }
        if src_rate == 0 || dst_rate == 0 {
            return Err(ResampleError::InvalidRates);
        }

        Ok(Self {
            src_rate,
            dst_rate,
            channels,
            pos: 1.0,
            step: src_rate as f64 / dst_rate as f64,
            prev_frame: vec![0.0; channels],
            have_prev: false,
        })
    }

    #[inline]
    pub fn src_rate(&self) -> u32 {
        self.src_rate
    }

    #[inline]
    pub fn dst_rate(&self) -> u32 {
        self.dst_rate
    }

    #[inline]
    pub fn channels(&self) -> usize {
        self.channels
    }

    pub fn reset(&mut self) {
        self.pos = 1.0;
        self.have_prev = false;
        self.prev_frame.fill(0.0);
    }

    /// Resample a block of interleaved audio into `out`.
    ///
    /// `out` is cleared; capacity is reused.
    pub fn process_interleaved(
        &mut self,
        input: &[f32],
        out: &mut Vec<f32>,
    ) -> Result<(), ResampleError> {
        out.clear();

        if input.is_empty() {
            return Ok(());
        }
        if input.len() % self.channels != 0 {
            return Err(ResampleError::InputLengthNotAligned {
                expected_multiple: self.channels,
            });
        }

        let frames_in = input.len() / self.channels;
        if frames_in == 0 {
            return Ok(());
        }

        if !self.have_prev {
            self.prev_frame.copy_from_slice(&input[..self.channels]);
            self.have_prev = true;
            self.pos = 1.0;
        }

        // Output estimate for reserving capacity.
        let ratio = self.dst_rate as f64 / self.src_rate as f64;
        let est_out_frames = ((frames_in as f64) * ratio).ceil() as usize + 2;
        out.reserve(est_out_frames * self.channels);

        let frames_in_f = frames_in as f64;

        while self.pos < frames_in_f {
            let i0 = self.pos.floor() as usize;
            let frac = (self.pos - (i0 as f64)) as f32;

            let frame0 = if i0 == 0 {
                &self.prev_frame[..]
            } else {
                let base0 = (i0 - 1) * self.channels;
                &input[base0..base0 + self.channels]
            };

            let base1 = i0 * self.channels;
            let frame1 = &input[base1..base1 + self.channels];

            // out = frame0 + (frame1 - frame0) * frac
            for c in 0..self.channels {
                let a = frame0[c];
                out.push(a + (frame1[c] - a) * frac);
            }

            self.pos += self.step;
        }

        // If we land exactly on the last input frame, we can output it without needing lookahead.
        let i0 = self.pos.floor() as usize;
        let frac = self.pos - (i0 as f64);
        if i0 == frames_in && frac == 0.0 {
            let last = &input[(frames_in - 1) * self.channels..frames_in * self.channels];
            out.extend_from_slice(last);
            self.pos += self.step;
        }

        // Update state for the next call: the last input frame becomes the new `prev_frame`.
        self.prev_frame
            .copy_from_slice(&input[(frames_in - 1) * self.channels..frames_in * self.channels]);

        // Rebase `pos` so that the new `prev_frame` is at index 0.
        self.pos -= frames_in_f;

        Ok(())
    }

    /// Flush the resampler at end-of-stream.
    ///
    /// This appends any remaining output that can be produced by extending the signal with
    /// a repeated last sample (zero-order hold).
    pub fn flush_interleaved(&mut self, out: &mut Vec<f32>) {
        out.clear();
        if !self.have_prev {
            return;
        }

        // With no future samples available, treat the next frame as identical to `prev_frame`.
        while self.pos < 1.0 {
            let frac = self.pos as f32;
            for c in 0..self.channels {
                let a = self.prev_frame[c];
                // frame1 == frame0, so interpolation is constant.
                out.push(a + (a - a) * frac);
            }
            self.pos += self.step;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_resampler_preserves_dc() {
        let mut r = LinearResampler::new(44_100, 48_000, 2).unwrap();

        let frames = 256;
        let mut input = Vec::with_capacity(frames * 2);
        for _ in 0..frames {
            input.push(0.25);
            input.push(0.25);
        }

        let mut out_a = Vec::new();
        let mut out_b = Vec::new();
        r.process_interleaved(&input, &mut out_a).unwrap();
        r.flush_interleaved(&mut out_b);
        out_a.extend_from_slice(&out_b);

        for s in out_a {
            assert!((s - 0.25).abs() < 1e-6);
        }
    }

    #[test]
    fn linear_resampler_identity_for_equal_rates() {
        let mut r = LinearResampler::new(48_000, 48_000, 1).unwrap();
        let input: Vec<f32> = (0..64).map(|x| x as f32 / 64.0).collect();

        let mut out = Vec::new();
        r.process_interleaved(&input, &mut out).unwrap();
        assert_eq!(out, input);

        let mut tail = Vec::new();
        r.flush_interleaved(&mut tail);
        assert!(tail.is_empty());
    }

    #[test]
    fn linear_resampler_sine_frequency_is_reasonable() {
        let src_rate = 44_100u32;
        let dst_rate = 48_000u32;
        let freq = 440.0f32;

        let mut r = LinearResampler::new(src_rate, dst_rate, 1).unwrap();

        let frames_in = (src_rate as usize / 10).max(1); // 100ms
        let mut input = Vec::with_capacity(frames_in);
        for n in 0..frames_in {
            let t = n as f32 / src_rate as f32;
            input.push((core::f32::consts::TAU * freq * t).sin());
        }

        let mut out = Vec::new();
        let mut tail = Vec::new();
        r.process_interleaved(&input, &mut out).unwrap();
        r.flush_interleaved(&mut tail);
        out.extend_from_slice(&tail);

        let frames_out = out.len();
        assert!(frames_out > 0);

        // Compare against an ideal sine at the target rate.
        let mut mse = 0.0f32;
        for (n, &s) in out.iter().enumerate() {
            let t = n as f32 / dst_rate as f32;
            let expected = (core::f32::consts::TAU * freq * t).sin();
            let diff = s - expected;
            mse += diff * diff;
        }
        let rmse = (mse / frames_out as f32).sqrt();
        assert!(rmse < 0.02, "rmse too high for linear resampler: {rmse}");
    }

    #[cfg(feature = "sinc-resampler")]
    #[test]
    fn sinc_resampler_preserves_dc() {
        let mut r = SincResampler::new(44_100, 48_000, 1).unwrap();

        let frames = 2048;
        let input = vec![0.25f32; frames];

        let mut out_a = Vec::new();
        let mut out_b = Vec::new();
        r.process_interleaved(&input, &mut out_a).unwrap();
        r.flush_interleaved(&mut out_b);
        out_a.extend_from_slice(&out_b);

        assert!(!out_a.is_empty());
        // The filter starts with zero history, so the first few outputs contain an
        // expected warm-up transient. Validate DC preservation in the steady state.
        let skip = 128usize.min(out_a.len());
        let tail = 128usize.min(out_a.len().saturating_sub(skip));
        let mid = &out_a[skip..out_a.len() - tail];
        assert!(!mid.is_empty());
        for &s in mid {
            assert!((s - 0.25).abs() < 1e-3);
        }
    }

    #[cfg(feature = "sinc-resampler")]
    #[test]
    fn sinc_resampler_handles_tiny_inputs_without_panicking() {
        let mut r = SincResampler::new(48_000, 44_100, 2).unwrap();

        // One stereo frame.
        let input = vec![0.1f32, -0.2f32];
        let mut out = Vec::new();
        let mut tail = Vec::new();
        r.process_interleaved(&input, &mut out).unwrap();
        r.flush_interleaved(&mut tail);
        out.extend_from_slice(&tail);

        // Output length depends on filter latency and ratio; just ensure the pipeline works.
        assert!(out.len() % 2 == 0);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResamplerKind {
    Linear,
    #[cfg(feature = "sinc-resampler")]
    Sinc,
}

pub enum Resampler {
    Linear(LinearResampler),
    #[cfg(feature = "sinc-resampler")]
    Sinc(SincResampler),
}

impl Resampler {
    pub fn new(
        kind: ResamplerKind,
        src_rate: u32,
        dst_rate: u32,
        channels: usize,
    ) -> Result<Self, ResampleError> {
        match kind {
            ResamplerKind::Linear => Ok(Self::Linear(LinearResampler::new(
                src_rate, dst_rate, channels,
            )?)),
            #[cfg(feature = "sinc-resampler")]
            ResamplerKind::Sinc => Ok(Self::Sinc(SincResampler::new(
                src_rate, dst_rate, channels,
            )?)),
        }
    }

    pub fn process_interleaved(
        &mut self,
        input: &[f32],
        out: &mut Vec<f32>,
    ) -> Result<(), ResampleError> {
        match self {
            Self::Linear(r) => r.process_interleaved(input, out),
            #[cfg(feature = "sinc-resampler")]
            Self::Sinc(r) => r.process_interleaved(input, out),
        }
    }

    pub fn flush_interleaved(&mut self, out: &mut Vec<f32>) {
        match self {
            Self::Linear(r) => r.flush_interleaved(out),
            #[cfg(feature = "sinc-resampler")]
            Self::Sinc(r) => r.flush_interleaved(out),
        }
    }

    pub fn reset(&mut self) {
        match self {
            Self::Linear(r) => r.reset(),
            #[cfg(feature = "sinc-resampler")]
            Self::Sinc(r) => r.reset(),
        }
    }
}

#[cfg(feature = "sinc-resampler")]
pub struct SincResampler {
    src_rate: u32,
    dst_rate: u32,
    channels: usize,

    pos: f64,
    step: f64,

    // Windowed-sinc filter config
    taps: usize,
    half: isize,
    phases: usize,
    table: Vec<f32>, // phases * taps

    // History of the last `taps` frames so we can evaluate the convolution near boundaries.
    history: Vec<f32>, // interleaved, length = history_frames * channels
    history_frames: usize,
}

#[cfg(feature = "sinc-resampler")]
impl SincResampler {
    pub fn new(src_rate: u32, dst_rate: u32, channels: usize) -> Result<Self, ResampleError> {
        if channels == 0 {
            return Err(ResampleError::InvalidChannels);
        }
        if src_rate == 0 || dst_rate == 0 {
            return Err(ResampleError::InvalidRates);
        }

        let taps = 32;
        let phases = 1024;
        let half = (taps as isize) / 2;

        let mut table = Vec::with_capacity(phases * taps);
        for p in 0..phases {
            let frac = p as f64 / phases as f64;
            let mut sum = 0.0f64;
            for t in 0..taps {
                // k runs from -half+1 .. half (inclusive)
                let k = (t as isize) - (half - 1);
                let x = (k as f64) - frac;
                let sinc = if x == 0.0 {
                    1.0
                } else {
                    let pix = core::f64::consts::PI * x;
                    pix.sin() / pix
                };

                // Hann window
                let w =
                    0.5 - 0.5 * (2.0 * core::f64::consts::PI * (t as f64) / (taps as f64)).cos();
                let coeff = sinc * w;
                sum += coeff;
                table.push(coeff as f32);
            }

            // Normalise each phase to preserve DC.
            let inv = (1.0 / sum) as f32;
            let start = p * taps;
            for v in &mut table[start..start + taps] {
                *v *= inv;
            }
        }

        Ok(Self {
            src_rate,
            dst_rate,
            channels,
            pos: 0.0,
            step: src_rate as f64 / dst_rate as f64,
            taps,
            half,
            phases,
            table,
            history: vec![0.0; taps * channels],
            history_frames: taps,
        })
    }

    pub fn reset(&mut self) {
        self.pos = 0.0;
        self.history.fill(0.0);
    }

    pub fn process_interleaved(
        &mut self,
        input: &[f32],
        out: &mut Vec<f32>,
    ) -> Result<(), ResampleError> {
        out.clear();

        if input.is_empty() {
            return Ok(());
        }
        if input.len() % self.channels != 0 {
            return Err(ResampleError::InputLengthNotAligned {
                expected_multiple: self.channels,
            });
        }

        let frames_in = input.len() / self.channels;
        if frames_in == 0 {
            return Ok(());
        }

        // We conceptually process over a virtual buffer:
        // [history (taps frames)] + [input frames]
        // where history holds the most recent `taps` frames (zero-filled initially).
        let total_frames = self.history_frames + frames_in;
        let total_frames_f = total_frames as f64;

        let ratio = self.dst_rate as f64 / self.src_rate as f64;
        let est_out_frames = ((frames_in as f64) * ratio).ceil() as usize + 2;
        out.reserve(est_out_frames * self.channels);

        while self.pos < total_frames_f {
            let center = self.pos.floor() as isize;
            let frac = self.pos - center as f64;
            let phase = ((frac * self.phases as f64).round() as usize).min(self.phases - 1);
            let coeffs = &self.table[phase * self.taps..(phase + 1) * self.taps];

            // Need samples from center-(half-1) .. center+half.
            let start = center - (self.half - 1);
            let end = center + self.half;

            // If we don't have enough lookahead, stop and wait for more input.
            if end >= total_frames as isize {
                break;
            }

            for c in 0..self.channels {
                let mut acc = 0.0f32;
                for (t, &k) in coeffs.iter().enumerate() {
                    let idx = start + t as isize;
                    let sample = if idx < 0 {
                        0.0
                    } else if idx < self.history_frames as isize {
                        let base = idx as usize * self.channels;
                        self.history[base + c]
                    } else {
                        let in_idx = (idx as usize - self.history_frames) * self.channels;
                        input[in_idx + c]
                    };
                    acc += sample * k;
                }
                out.push(acc);
            }

            self.pos += self.step;
        }

        // Update history with the last `taps` frames of (history + input).
        if frames_in >= self.taps {
            let tail = &input[(frames_in - self.taps) * self.channels..frames_in * self.channels];
            self.history.copy_from_slice(tail);
        } else {
            // Shift left and append.
            let shift_frames = self.taps - frames_in;
            self.history.copy_within(frames_in * self.channels.., 0);
            self.history[shift_frames * self.channels..].copy_from_slice(input);
        }

        // Rebase position so that the new history starts at 0.
        self.pos -= frames_in as f64;

        Ok(())
    }

    pub fn flush_interleaved(&mut self, out: &mut Vec<f32>) {
        out.clear();
        if self.channels == 0 {
            return;
        }
        if self.history_frames == 0 {
            return;
        }
        let last_frame = &self.history
            [(self.history_frames - 1) * self.channels..self.history_frames * self.channels];

        // At end-of-stream we can extend the signal by repeating the last sample to
        // generate the final filter tail. Stop once the resample position moves
        // past the last real frame in `history`.
        let history_frames_f = self.history_frames as f64;
        while self.pos < history_frames_f {
            let center = self.pos.floor() as isize;
            let frac = self.pos - center as f64;
            let phase = ((frac * self.phases as f64).round() as usize).min(self.phases - 1);
            let coeffs = &self.table[phase * self.taps..(phase + 1) * self.taps];

            // Need samples from center-(half-1) .. center+half.
            let start = center - (self.half - 1);

            for c in 0..self.channels {
                let mut acc = 0.0f32;
                for (t, &k) in coeffs.iter().enumerate() {
                    let idx = start + t as isize;
                    let sample = if idx < 0 {
                        0.0
                    } else if idx < self.history_frames as isize {
                        let base = idx as usize * self.channels;
                        self.history[base + c]
                    } else {
                        last_frame[c]
                    };
                    acc += sample * k;
                }
                out.push(acc);
            }

            self.pos += self.step;
        }
    }
}
