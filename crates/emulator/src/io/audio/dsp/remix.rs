use core::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemixError {
    InvalidChannels,
    InputLengthNotAligned { expected_multiple: usize },
}

impl fmt::Display for RemixError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidChannels => write!(f, "invalid channel count"),
            Self::InputLengthNotAligned { expected_multiple } => write!(
                f,
                "remix input length is not a multiple of {expected_multiple} samples"
            ),
        }
    }
}

impl std::error::Error for RemixError {}

#[inline]
fn ensure_capacity_and_len(out: &mut Vec<f32>, len: usize) {
    out.clear();
    if out.capacity() < len {
        out.reserve(len - out.capacity());
    }
    // SAFETY: the caller writes every element below.
    unsafe {
        out.set_len(len);
    }
}

/// Remix interleaved samples from `src_channels` to `dst_channels`.
///
/// Supported fast paths:
/// - mono ↔ stereo
/// - 5.1 (FL,FR,FC,LFE,SL,SR) → stereo
pub fn remix_interleaved(
    input: &[f32],
    src_channels: usize,
    dst_channels: usize,
    out: &mut Vec<f32>,
) -> Result<(), RemixError> {
    if src_channels == 0 || dst_channels == 0 {
        return Err(RemixError::InvalidChannels);
    }
    if input.len() % src_channels != 0 {
        return Err(RemixError::InputLengthNotAligned {
            expected_multiple: src_channels,
        });
    }

    let frames = input.len() / src_channels;
    let out_len = frames * dst_channels;
    ensure_capacity_and_len(out, out_len);

    if src_channels == dst_channels {
        out.copy_from_slice(input);
        return Ok(());
    }

    match (src_channels, dst_channels) {
        (1, 2) => {
            for i in 0..frames {
                let s = input[i];
                out[i * 2] = s;
                out[i * 2 + 1] = s;
            }
        }
        (2, 1) => {
            for i in 0..frames {
                let base = i * 2;
                out[i] = (input[base] + input[base + 1]) * 0.5;
            }
        }
        (6, 2) => {
            const C: f32 = 0.707_106_77; // -3 dB (sqrt(1/2))
            const LFE: f32 = 0.5;
            for i in 0..frames {
                let base = i * 6;
                let fl = input[base];
                let fr = input[base + 1];
                let fc = input[base + 2];
                let lfe = input[base + 3];
                let sl = input[base + 4];
                let sr = input[base + 5];

                out[i * 2] = fl + fc * C + sl * C + lfe * LFE;
                out[i * 2 + 1] = fr + fc * C + sr * C + lfe * LFE;
            }
        }
        (2, 6) => {
            for i in 0..frames {
                let base_in = i * 2;
                let l = input[base_in];
                let r = input[base_in + 1];
                let base_out = i * 6;

                out[base_out] = l;
                out[base_out + 1] = r;
                out[base_out + 2] = (l + r) * 0.5;
                out[base_out + 3] = 0.0;
                out[base_out + 4] = l;
                out[base_out + 5] = r;
            }
        }
        _ => {
            // Generic fallbacks:
            if dst_channels == 1 {
                let inv = 1.0 / (src_channels as f32);
                for i in 0..frames {
                    let base = i * src_channels;
                    let mut acc = 0.0;
                    for c in 0..src_channels {
                        acc += input[base + c];
                    }
                    out[i] = acc * inv;
                }
            } else if src_channels == 1 {
                for i in 0..frames {
                    let s = input[i];
                    let base_out = i * dst_channels;
                    for c in 0..dst_channels {
                        out[base_out + c] = s;
                    }
                }
            } else if dst_channels == 2 {
                // Heuristic: preserve first two channels and fold the rest in at half gain.
                for i in 0..frames {
                    let base = i * src_channels;
                    let mut l = input[base];
                    let mut r = input[base + 1];
                    for c in 2..src_channels {
                        let v = input[base + c] * 0.5;
                        l += v;
                        r += v;
                    }
                    let base_out = i * 2;
                    out[base_out] = l;
                    out[base_out + 1] = r;
                }
            } else {
                // Truncate or zero-extend.
                let copy_channels = src_channels.min(dst_channels);
                for i in 0..frames {
                    let base_in = i * src_channels;
                    let base_out = i * dst_channels;
                    for c in 0..copy_channels {
                        out[base_out + c] = input[base_in + c];
                    }
                    for c in copy_channels..dst_channels {
                        out[base_out + c] = 0.0;
                    }
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mono_to_stereo_duplicates() {
        let input = [0.25f32, -0.5];
        let mut out = Vec::new();
        remix_interleaved(&input, 1, 2, &mut out).unwrap();
        assert_eq!(out, vec![0.25, 0.25, -0.5, -0.5]);
    }

    #[test]
    fn stereo_to_mono_averages() {
        let input = [1.0f32, -1.0, 0.5, 0.25];
        let mut out = Vec::new();
        remix_interleaved(&input, 2, 1, &mut out).unwrap();
        assert_eq!(out.len(), 2);
        assert!((out[0] - 0.0).abs() < 1e-6);
        assert!((out[1] - 0.375).abs() < 1e-6);
    }

    #[test]
    fn downmix_5_1_to_stereo() {
        const C: f32 = 0.707_106_77;
        const LFE: f32 = 0.5;

        // One 5.1 frame: FL, FR, FC, LFE, SL, SR
        let input = [1.0f32, -1.0, 0.5, 0.25, 0.75, -0.5];
        let mut out = Vec::new();
        remix_interleaved(&input, 6, 2, &mut out).unwrap();
        assert_eq!(out.len(), 2);

        let expected_l = 1.0 + 0.5 * C + 0.75 * C + 0.25 * LFE;
        let expected_r = -1.0 + 0.5 * C + (-0.5) * C + 0.25 * LFE;
        assert!((out[0] - expected_l).abs() < 1e-6);
        assert!((out[1] - expected_r).abs() < 1e-6);
    }
}
