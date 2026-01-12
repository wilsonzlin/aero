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
    if !input.len().is_multiple_of(src_channels) {
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
            for (i, &s) in input.iter().enumerate() {
                let base_out = i * 2;
                out[base_out] = s;
                out[base_out + 1] = s;
            }
        }
        (2, 1) => {
            for (i, frame) in input.chunks_exact(2).enumerate() {
                out[i] = (frame[0] + frame[1]) * 0.5;
            }
        }
        (6, 2) => {
            const C: f32 = 0.707_106_77; // -3 dB (sqrt(1/2))
            const LFE: f32 = 0.5;
            for (i, frame) in input.chunks_exact(6).enumerate() {
                let fl = frame[0];
                let fr = frame[1];
                let fc = frame[2];
                let lfe = frame[3];
                let sl = frame[4];
                let sr = frame[5];

                let base_out = i * 2;
                out[base_out] = fl + fc * C + sl * C + lfe * LFE;
                out[base_out + 1] = fr + fc * C + sr * C + lfe * LFE;
            }
        }
        (2, 6) => {
            for (i, frame) in input.chunks_exact(2).enumerate() {
                let l = frame[0];
                let r = frame[1];
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
                for (i, frame) in input.chunks_exact(src_channels).enumerate() {
                    out[i] = frame.iter().sum::<f32>() * inv;
                }
            } else if src_channels == 1 {
                for (i, &s) in input.iter().enumerate() {
                    let base_out = i * dst_channels;
                    out[base_out..base_out + dst_channels].fill(s);
                }
            } else if dst_channels == 2 {
                // Heuristic: preserve first two channels and fold the rest in at half gain.
                for (i, frame) in input.chunks_exact(src_channels).enumerate() {
                    let mut l = frame[0];
                    let mut r = frame[1];
                    for &sample in &frame[2..] {
                        let v = sample * 0.5;
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
                for (i, frame) in input.chunks_exact(src_channels).enumerate() {
                    let base_out = i * dst_channels;
                    out[base_out..base_out + copy_channels]
                        .copy_from_slice(&frame[..copy_channels]);
                    out[base_out + copy_channels..base_out + dst_channels].fill(0.0);
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
