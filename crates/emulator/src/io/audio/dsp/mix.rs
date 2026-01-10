use core::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MixError {
    InvalidChannels,
    StreamLengthMismatch,
    InputLengthNotAligned { expected_multiple: usize },
}

impl fmt::Display for MixError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidChannels => write!(f, "invalid channel count"),
            Self::StreamLengthMismatch => write!(f, "stream length mismatch"),
            Self::InputLengthNotAligned { expected_multiple } => write!(
                f,
                "mix input length is not a multiple of {expected_multiple} samples"
            ),
        }
    }
}

impl std::error::Error for MixError {}

pub struct Mixer {
    channels: usize,
    clamp: bool,
}

impl Mixer {
    pub fn new(channels: usize) -> Result<Self, MixError> {
        if channels == 0 {
            return Err(MixError::InvalidChannels);
        }
        Ok(Self {
            channels,
            clamp: true,
        })
    }

    /// Enable/disable hard clipping to `[-1.0, 1.0]` after mixing.
    pub fn with_clamp(mut self, clamp: bool) -> Self {
        self.clamp = clamp;
        self
    }

    /// Mix multiple interleaved streams of equal length into `out`.
    ///
    /// `out` is cleared and overwritten.
    pub fn mix_interleaved(&self, streams: &[&[f32]], out: &mut Vec<f32>) -> Result<(), MixError> {
        out.clear();
        if streams.is_empty() {
            return Ok(());
        }

        let len = streams[0].len();
        if len % self.channels != 0 {
            return Err(MixError::InputLengthNotAligned {
                expected_multiple: self.channels,
            });
        }
        for s in streams.iter().skip(1) {
            if s.len() != len {
                return Err(MixError::StreamLengthMismatch);
            }
            if s.len() % self.channels != 0 {
                return Err(MixError::InputLengthNotAligned {
                    expected_multiple: self.channels,
                });
            }
        }

        out.resize(len, 0.0);

        for &s in streams {
            add_scaled_in_place(&mut out[..], s, 1.0);
        }

        if self.clamp {
            clamp_in_place(&mut out[..]);
        }

        Ok(())
    }
}

#[inline]
pub fn clamp_in_place(samples: &mut [f32]) {
    for s in samples {
        *s = s.clamp(-1.0, 1.0);
    }
}

#[inline]
pub fn add_scaled_in_place(dst: &mut [f32], src: &[f32], gain: f32) {
    debug_assert_eq!(dst.len(), src.len());

    #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
    {
        use core::arch::wasm32::{f32x4_add, f32x4_mul, f32x4_splat, v128, v128_load, v128_store};

        let mut i = 0;
        let gain_v = f32x4_splat(gain);

        // Process 4 samples at a time.
        while i + 4 <= dst.len() {
            unsafe {
                let a = v128_load(dst.as_ptr().add(i) as *const v128);
                let b = v128_load(src.as_ptr().add(i) as *const v128);
                let res = f32x4_add(a, f32x4_mul(b, gain_v));
                v128_store(dst.as_mut_ptr().add(i) as *mut v128, res);
            }
            i += 4;
        }

        // Tail.
        while i < dst.len() {
            dst[i] += src[i] * gain;
            i += 1;
        }

        return;
    }

    // Scalar fallback.
    for (d, s) in dst.iter_mut().zip(src.iter()) {
        *d += *s * gain;
    }
}

/// Simple TPDF dither source (xorshift64*).
///
/// Dithering is only relevant when quantising float → integer; it is provided as a building
/// block for future sink conversions.
pub struct Dither {
    state: u64,
}

impl Dither {
    pub fn new(seed: u64) -> Self {
        let seed = if seed == 0 {
            0x9E37_79B9_7F4A_7C15
        } else {
            seed
        };
        Self { state: seed }
    }

    #[inline]
    fn next_u32(&mut self) -> u32 {
        // xorshift64*
        let mut x = self.state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.state = x;
        ((x.wrapping_mul(0x2545_F491_4F6C_DD1D)) >> 32) as u32
    }

    #[inline]
    fn next_f32(&mut self) -> f32 {
        // [0, 1)
        (self.next_u32() as f32) * (1.0 / (u32::MAX as f32 + 1.0))
    }

    /// Generate TPDF noise in `[-1, 1]`.
    #[inline]
    pub fn next_tpdf(&mut self) -> f32 {
        (self.next_f32() + self.next_f32()) - 1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixer_sums_streams_and_clamps() {
        let mixer = Mixer::new(2).unwrap();
        let a = [0.75f32, 0.75, 0.75, 0.75];
        let b = [0.75f32, -0.5, 0.1, 0.3];

        let mut out = Vec::new();
        mixer.mix_interleaved(&[&a, &b], &mut out).unwrap();
        assert_eq!(out.len(), 4);
        assert_eq!(out[0], 1.0); // 1.5 clamped
        assert!((out[1] - 0.25).abs() < 1e-6);
        assert!((out[2] - 0.85).abs() < 1e-6);
        assert_eq!(out[3], 1.0); // 1.05 clamped
    }

    #[test]
    fn dither_produces_noise_in_range() {
        let mut d = Dither::new(1);
        for _ in 0..1000 {
            let v = d.next_tpdf();
            assert!(v >= -1.0 && v <= 1.0);
        }
    }
}
