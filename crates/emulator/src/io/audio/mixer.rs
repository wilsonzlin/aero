/// Convert signed 16-bit PCM samples to normalized `f32` in `[-1.0, 1.0)`.
#[inline]
pub fn pcm_i16_to_f32(sample: i16) -> f32 {
    // `i16::MIN` maps to -1.0, `i16::MAX` maps to just under +1.0.
    sample as f32 / 32768.0
}

/// Convert interleaved stereo `i16` PCM into interleaved stereo `f32`.
///
/// Any trailing odd sample is ignored.
pub fn convert_i16_stereo_to_f32_interleaved(pcm: &[i16]) -> Vec<f32> {
    let frames = pcm.len() / 2;
    let mut out = Vec::with_capacity(frames * 2);

    for frame in 0..frames {
        let l = pcm[frame * 2];
        let r = pcm[frame * 2 + 1];
        out.push(pcm_i16_to_f32(l));
        out.push(pcm_i16_to_f32(r));
    }

    out
}

/// Naive linear-resampling for interleaved stereo buffers.
///
/// This is intended as a simple correctness-first resampler for bridging guest
/// sample rates (44.1kHz/48kHz) to the browser `AudioContext` rate (typically
/// 48kHz). A real-time emulator will likely want a higher-quality and
/// stateful resampler, but this covers the basic requirement and is easy to
/// validate.
pub fn resample_linear_stereo_interleaved(
    input: &[f32],
    src_rate_hz: u32,
    dst_rate_hz: u32,
) -> Vec<f32> {
    if src_rate_hz == 0 || dst_rate_hz == 0 {
        return Vec::new();
    }

    if src_rate_hz == dst_rate_hz {
        return input.to_vec();
    }

    let src_frames = input.len() / 2;
    if src_frames == 0 {
        return Vec::new();
    }

    let ratio = dst_rate_hz as f64 / src_rate_hz as f64;
    let dst_frames = (src_frames as f64 * ratio).floor() as usize;
    let mut out = Vec::with_capacity(dst_frames * 2);

    for dst_i in 0..dst_frames {
        // Map output frame index into source frame space.
        let src_pos = dst_i as f64 / ratio;
        let src_i0 = src_pos.floor() as usize;
        let frac = (src_pos - src_i0 as f64) as f32;

        let src_i1 = (src_i0 + 1).min(src_frames - 1);

        let l0 = input[src_i0 * 2];
        let r0 = input[src_i0 * 2 + 1];
        let l1 = input[src_i1 * 2];
        let r1 = input[src_i1 * 2 + 1];

        out.push(l0 + (l1 - l0) * frac);
        out.push(r0 + (r1 - r0) * frac);
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn i16_conversion_matches_expected_range() {
        assert_eq!(pcm_i16_to_f32(i16::MIN), -1.0);
        // i16::MAX maps to just under 1.0.
        assert!(pcm_i16_to_f32(i16::MAX) < 1.0);
    }

    #[test]
    fn resample_identity_returns_copy() {
        let input = vec![0.0f32, 0.0, 1.0, 1.0];
        let out = resample_linear_stereo_interleaved(&input, 48_000, 48_000);
        assert_eq!(out, input);
    }
}
