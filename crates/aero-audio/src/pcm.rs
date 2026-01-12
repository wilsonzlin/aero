use std::collections::VecDeque;

/// Parsed HDA stream format (from `SDnFMT`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamFormat {
    pub sample_rate_hz: u32,
    pub bits_per_sample: u8,
    pub channels: u8,
}

impl StreamFormat {
    pub fn from_hda_format(fmt: u16) -> Self {
        // HDA format register encoding (Intel HDA Spec):
        // 15    : stream type (0 = PCM, 1 = non-PCM) - ignored for now
        // 14    : base rate (0 = 48kHz, 1 = 44.1kHz)
        // 13:11 : rate multiplier
        // 10:8  : rate divisor
        // 7:4   : bits per sample encoding
        // 3:0   : channels - 1
        let base = if (fmt & (1 << 14)) != 0 {
            44_100
        } else {
            48_000
        };
        let mult = match (fmt >> 11) & 0x7 {
            0 => 1,
            1 => 2,
            2 => 3,
            3 => 4,
            _ => 1,
        };
        let div = match (fmt >> 8) & 0x7 {
            0 => 1,
            1 => 2,
            2 => 3,
            3 => 4,
            4 => 5,
            5 => 6,
            6 => 7,
            7 => 8,
            _ => 1,
        };

        let bits = match (fmt >> 4) & 0x7 {
            0 => 8,
            1 => 16,
            2 => 20,
            3 => 24,
            4 => 32,
            _ => 16,
        };

        let channels = ((fmt & 0xF) + 1) as u8;

        Self {
            sample_rate_hz: (base * mult) / div,
            bits_per_sample: bits,
            channels,
        }
    }

    pub fn bytes_per_sample(&self) -> usize {
        match self.bits_per_sample {
            8 => 1,
            16 => 2,
            20 | 24 | 32 => 4,
            other => panic!("unsupported bits per sample: {other}"),
        }
    }

    pub fn bytes_per_frame(&self) -> usize {
        self.bytes_per_sample() * self.channels as usize
    }
}

/// Decode interleaved PCM data from the guest into interleaved stereo `f32`.
///
/// Any channel count other than 1 or >=2 is mapped to stereo by:
/// - mono: duplicated to L/R
/// - >=2: first two channels
pub fn decode_pcm_to_stereo_f32(input: &[u8], fmt: StreamFormat) -> Vec<[f32; 2]> {
    let mut out = Vec::new();
    decode_pcm_to_stereo_f32_into(input, fmt, &mut out);
    out
}

/// Decode interleaved PCM data from the guest into a caller-provided output buffer.
///
/// `out` is cleared before writing.
pub fn decode_pcm_to_stereo_f32_into(input: &[u8], fmt: StreamFormat, out: &mut Vec<[f32; 2]>) {
    let bytes_per_frame = fmt.bytes_per_frame();
    out.clear();
    if bytes_per_frame == 0 {
        return;
    }
    let frames = input.len() / bytes_per_frame;
    out.reserve(frames);

    for frame in 0..frames {
        let frame_off = frame * bytes_per_frame;
        let chan = |ch: u8| -> f32 {
            let ch = ch as usize;
            let off = frame_off + ch * fmt.bytes_per_sample();
            decode_one_sample(
                &input[off..off + fmt.bytes_per_sample()],
                fmt.bits_per_sample,
            )
        };

        let l = if fmt.channels > 0 { chan(0) } else { 0.0 };
        let r = if fmt.channels == 1 {
            l
        } else if fmt.channels >= 2 {
            chan(1)
        } else {
            0.0
        };
        out.push([l, r]);
    }
}

/// Encode mono `f32` samples into interleaved PCM bytes as described by `fmt`.
///
/// Channel mapping:
/// - 1 channel: mono
/// - 2+ channels: the mono signal is duplicated into the first two channels and
///   remaining channels are filled with silence.
pub fn encode_mono_f32_to_pcm(input: &[f32], fmt: StreamFormat) -> Vec<u8> {
    let mut out = Vec::new();
    encode_mono_f32_to_pcm_into(input, fmt, &mut out);
    out
}

/// Encode mono `f32` samples into interleaved PCM bytes as described by `fmt`, writing into `out`.
///
/// `out` is cleared before writing.
pub fn encode_mono_f32_to_pcm_into(input: &[f32], fmt: StreamFormat, out: &mut Vec<u8>) {
    let bytes_per_frame = fmt.bytes_per_frame();
    out.clear();
    if bytes_per_frame == 0 {
        return;
    }

    out.resize(input.len() * bytes_per_frame, 0);
    let bps = fmt.bytes_per_sample();
    let channels = fmt.channels as usize;

    for (frame_idx, &mono) in input.iter().enumerate() {
        let frame_off = frame_idx * bytes_per_frame;
        for ch in 0..channels {
            let sample = if ch <= 1 { mono } else { 0.0 };
            let off = frame_off + ch * bps;
            encode_one_sample(&mut out[off..off + bps], fmt.bits_per_sample, sample);
        }
    }
}

fn decode_one_sample(bytes: &[u8], bits_per_sample: u8) -> f32 {
    match bits_per_sample {
        8 => (bytes[0] as f32 - 128.0) / 128.0,
        16 => {
            let v = i16::from_le_bytes([bytes[0], bytes[1]]) as f32;
            v / 32768.0
        }
        20 => {
            let raw = i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
            // Treat as signed 20-bit value stored in the low bits.
            let v = (raw << 12) >> 12; // sign-extend low 20 bits
            v as f32 / 524_288.0 // 2^19
        }
        24 => {
            let raw = i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
            let v = (raw << 8) >> 8; // sign-extend low 24 bits
            v as f32 / 8_388_608.0 // 2^23
        }
        32 => {
            let v = i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as f32;
            v / 2_147_483_648.0
        }
        other => {
            let _ = other;
            0.0
        }
    }
}

fn encode_one_sample(out: &mut [u8], bits_per_sample: u8, sample: f32) {
    match bits_per_sample {
        8 => {
            // 8-bit PCM is unsigned with a 128 bias.
            let v = (sample.clamp(-1.0, 1.0) * 128.0 + 128.0).round();
            let v = v.clamp(0.0, 255.0) as u8;
            out[0] = v;
        }
        16 => {
            let v = (sample.clamp(-1.0, 1.0) * 32768.0).round();
            let v = v.clamp(i16::MIN as f32, i16::MAX as f32) as i16;
            out.copy_from_slice(&v.to_le_bytes());
        }
        20 => {
            let v = (sample.clamp(-1.0, 1.0) * 524_288.0).round();
            let v = v.clamp(-524_288.0, 524_287.0) as i32;
            out.copy_from_slice(&v.to_le_bytes());
        }
        24 => {
            let v = (sample.clamp(-1.0, 1.0) * 8_388_608.0).round();
            let v = v.clamp(-8_388_608.0, 8_388_607.0) as i32;
            out.copy_from_slice(&v.to_le_bytes());
        }
        32 => {
            let v = (sample.clamp(-1.0, 1.0) * 2_147_483_648.0).round();
            let v = v.clamp(i32::MIN as f32, i32::MAX as f32) as i32;
            out.copy_from_slice(&v.to_le_bytes());
        }
        other => {
            let _ = other;
            out.fill(0);
        }
    }
}

/// Streaming linear resampler for interleaved stereo frames.
///
/// This is intentionally simple (linear interpolation), but it is stateful so
/// the HDA stream processing can advance guest DMA at the correct rate.
#[derive(Debug, Clone)]
pub struct LinearResampler {
    src_rate_hz: u32,
    dst_rate_hz: u32,
    step_src_per_dst: f64,
    src_pos: f64,
    src: VecDeque<[f32; 2]>,
}

impl LinearResampler {
    pub fn new(src_rate_hz: u32, dst_rate_hz: u32) -> Self {
        let mut this = Self {
            src_rate_hz,
            dst_rate_hz,
            step_src_per_dst: 1.0,
            src_pos: 0.0,
            src: VecDeque::new(),
        };
        this.recompute_step();
        this
    }

    pub fn reset_rates(&mut self, src_rate_hz: u32, dst_rate_hz: u32) {
        self.src_rate_hz = src_rate_hz;
        self.dst_rate_hz = dst_rate_hz;
        self.recompute_step();
        self.src_pos = 0.0;
        self.src.clear();
    }

    pub fn src_rate_hz(&self) -> u32 {
        self.src_rate_hz
    }

    pub fn dst_rate_hz(&self) -> u32 {
        self.dst_rate_hz
    }

    pub fn queued_source_frames(&self) -> usize {
        self.src.len()
    }

    pub fn push_source_frames(&mut self, frames: &[[f32; 2]]) {
        self.src.extend(frames.iter().copied());
    }

    /// Returns the minimum number of queued source frames required to be able to
    /// generate `dst_frames` output frames.
    pub fn required_source_frames(&self, dst_frames: usize) -> usize {
        if dst_frames == 0 {
            return 0;
        }
        // Need idx and idx+1 for the final output frame.
        let last_pos = self.src_pos + (dst_frames - 1) as f64 * self.step_src_per_dst;
        let idx = last_pos.floor() as usize;
        let frac = last_pos - idx as f64;
        if frac.abs() <= 1e-12 {
            idx.saturating_add(1)
        } else {
            idx.saturating_add(2)
        }
    }

    /// Produce up to `dst_frames` output frames, returning interleaved stereo.
    pub fn produce_interleaved_stereo(&mut self, dst_frames: usize) -> Vec<f32> {
        let mut out = Vec::with_capacity(dst_frames * 2);
        let _ = self.produce_interleaved_stereo_into(dst_frames, &mut out);
        out
    }

    /// Produce up to `dst_frames` output frames, writing them into `out` as interleaved stereo.
    ///
    /// `out` is cleared before writing.
    ///
    /// Returns the number of frames produced (which may be less than `dst_frames` if the source
    /// buffer does not contain enough data).
    pub fn produce_interleaved_stereo_into(
        &mut self,
        dst_frames: usize,
        out: &mut Vec<f32>,
    ) -> usize {
        out.clear();
        if dst_frames == 0 {
            return 0;
        }
        out.reserve(dst_frames * 2);

        let mut produced = 0usize;
        for _ in 0..dst_frames {
            if !self.produce_one_frame(out) {
                break;
            }
            produced += 1;
        }
        produced
    }

    /// Produce as many output frames as possible, writing them into `out` as interleaved stereo.
    ///
    /// `out` is cleared before writing.
    ///
    /// Returns the number of frames produced.
    pub fn produce_available_interleaved_stereo_into(&mut self, out: &mut Vec<f32>) -> usize {
        out.clear();
        let mut produced = 0usize;
        while self.produce_one_frame(out) {
            produced += 1;
        }
        produced
    }

    fn produce_one_frame(&mut self, out: &mut Vec<f32>) -> bool {
        let idx = self.src_pos.floor() as usize;
        let frac = self.src_pos - idx as f64;
        let a = match self.src.get(idx) {
            Some(v) => *v,
            None => return false,
        };
        let (l, r) = if frac.abs() <= 1e-12 {
            (a[0], a[1])
        } else {
            let b = match self.src.get(idx + 1) {
                Some(v) => *v,
                None => return false,
            };
            (lerp(a[0], b[0], frac as f32), lerp(a[1], b[1], frac as f32))
        };
        out.push(l);
        out.push(r);
        self.src_pos += self.step_src_per_dst;
        self.drop_consumed();
        true
    }

    fn drop_consumed(&mut self) {
        let drop = self.src_pos.floor() as usize;
        if drop == 0 {
            return;
        }
        for _ in 0..drop {
            let _ = self.src.pop_front();
        }
        self.src_pos -= drop as f64;
    }

    fn recompute_step(&mut self) {
        self.step_src_per_dst = self.src_rate_hz as f64 / self.dst_rate_hz as f64;
    }
}

#[cfg(feature = "io-snapshot")]
impl LinearResampler {
    pub(crate) fn snapshot_src_pos_bits(&self) -> u64 {
        self.src_pos.to_bits()
    }

    pub(crate) fn restore_snapshot_state(
        &mut self,
        src_rate_hz: u32,
        dst_rate_hz: u32,
        src_pos_bits: u64,
        queued_frames: u32,
    ) {
        self.reset_rates(src_rate_hz, dst_rate_hz);
        let src_pos = f64::from_bits(src_pos_bits);
        // `src_pos` is expected to be a small fractional position (<1.0) because we always drop
        // consumed frames as output is produced. Clamp corrupted/untrusted snapshot values so we
        // don't propagate NaNs/Infs into the resampling loop.
        self.src_pos = if src_pos.is_finite() && (0.0..1.0).contains(&src_pos) {
            src_pos
        } else {
            0.0
        };

        // Restoring `queued_frames` preserves guest-visible DMA determinism because it keeps the
        // resampler's "how far ahead of DMA are we?" bookkeeping stable. The actual queued audio
        // frames are not serialized, so we rehydrate them as silence.
        //
        // Snapshot files may come from untrusted sources; clamp the allocation to avoid OOM.
        const MAX_RESTORED_QUEUED_FRAMES: u32 = 65_536;
        let queued_frames = queued_frames.min(MAX_RESTORED_QUEUED_FRAMES);
        self.src
            .extend(std::iter::repeat_n([0.0, 0.0], queued_frames as usize));
    }
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_f32_approx_eq(actual: f32, expected: f32, tol: f32) {
        let diff = (actual - expected).abs();
        assert!(
            diff <= tol,
            "expected {expected}, got {actual} (|diff|={diff} > {tol})"
        );
    }

    #[test]
    fn parse_hda_format_48k_16bit_stereo() {
        // base=48k (bit14=0), mult=1, div=1, bits=16 (code 1), channels=2 -> fmt low nibble = 1.
        let fmt = (1 << 4) | 1;
        let parsed = StreamFormat::from_hda_format(fmt);
        assert_eq!(
            parsed,
            StreamFormat {
                sample_rate_hz: 48_000,
                bits_per_sample: 16,
                channels: 2
            }
        );
        assert_eq!(parsed.bytes_per_sample(), 2);
        assert_eq!(parsed.bytes_per_frame(), 4);
    }

    #[test]
    fn parse_hda_format_44k1_base_rate_and_mult_div() {
        // base=44.1k (bit14=1), mult=2 (code 1), div=1 (code 0), bits=24 (code 3), channels=1.
        let fmt = (1 << 14) | (1 << 11) | (3 << 4) | 0;
        let parsed = StreamFormat::from_hda_format(fmt);
        assert_eq!(
            parsed,
            StreamFormat {
                sample_rate_hz: 88_200,
                bits_per_sample: 24,
                channels: 1,
            }
        );

        // base=48k, mult=3 (code 2), div=2 (code 1), bits=20 (code 2), channels=4.
        let fmt = (2 << 11) | (1 << 8) | (2 << 4) | 3;
        let parsed = StreamFormat::from_hda_format(fmt);
        assert_eq!(
            parsed,
            StreamFormat {
                sample_rate_hz: 72_000,
                bits_per_sample: 20,
                channels: 4,
            }
        );
    }

    #[test]
    fn parse_hda_format_bits_per_sample_code_mapping() {
        // base=48k, mult/div=1, channels=1.
        let cases: &[(u16, u8, usize)] = &[
            (0, 8, 1),
            (1, 16, 2),
            (2, 20, 4),
            (3, 24, 4),
            (4, 32, 4),
        ];
        for &(code, bits, bps) in cases {
            let fmt = (code << 4) | 0;
            let parsed = StreamFormat::from_hda_format(fmt);
            assert_eq!(parsed.sample_rate_hz, 48_000);
            assert_eq!(parsed.bits_per_sample, bits);
            assert_eq!(parsed.channels, 1);
            assert_eq!(parsed.bytes_per_sample(), bps);
        }
    }

    #[test]
    fn parse_hda_format_reserved_codes_fall_back_safely() {
        // Reserved multiplier codes fall back to 1x.
        let fmt = (4 << 11) | (1 << 4) | 0;
        let parsed = StreamFormat::from_hda_format(fmt);
        assert_eq!(parsed.sample_rate_hz, 48_000);

        // Divisor codes cover 1..=8; spot-check the upper end.
        // base=48k, mult=4 (code 3), div=8 (code 7) -> 24k.
        let fmt = (3 << 11) | (7 << 8) | (1 << 4) | 0;
        let parsed = StreamFormat::from_hda_format(fmt);
        assert_eq!(parsed.sample_rate_hz, 24_000);

        // Reserved bits-per-sample codes fall back to 16-bit.
        let fmt = (5 << 4) | 0;
        let parsed = StreamFormat::from_hda_format(fmt);
        assert_eq!(parsed.bits_per_sample, 16);
        assert_eq!(parsed.bytes_per_sample(), 2);
    }

    #[test]
    #[should_panic(expected = "unsupported bits per sample")]
    fn bytes_per_sample_panics_for_unsupported_bits() {
        let fmt = StreamFormat {
            sample_rate_hz: 48_000,
            bits_per_sample: 12,
            channels: 2,
        };
        let _ = fmt.bytes_per_sample();
    }

    #[test]
    fn decode_encode_are_noops_for_zero_channel_format() {
        let fmt = StreamFormat {
            sample_rate_hz: 48_000,
            bits_per_sample: 16,
            channels: 0,
        };
        assert_eq!(fmt.bytes_per_frame(), 0);

        let mut out_frames = vec![[1.0, 2.0]];
        decode_pcm_to_stereo_f32_into(&[0u8; 16], fmt, &mut out_frames);
        assert!(out_frames.is_empty(), "out must be cleared on early return");

        let mut out_bytes = vec![1u8, 2, 3];
        encode_mono_f32_to_pcm_into(&[0.25, -0.25], fmt, &mut out_bytes);
        assert!(out_bytes.is_empty(), "out must be cleared on early return");
    }

    #[test]
    fn pcm_decode_8bit_unsigned_bias() {
        assert_f32_approx_eq(decode_one_sample(&[0], 8), -1.0, 1e-6);
        assert_f32_approx_eq(decode_one_sample(&[128], 8), 0.0, 1e-6);
        assert_f32_approx_eq(decode_one_sample(&[255], 8), 127.0 / 128.0, 1e-6);
    }

    #[test]
    fn pcm_8bit_encode_bias_rounding_and_clipping() {
        fn encode_u8(sample: f32) -> u8 {
            let mut out = [0u8; 1];
            encode_one_sample(&mut out, 8, sample);
            out[0]
        }

        assert_eq!(encode_u8(-1.0), 0);
        assert_eq!(encode_u8(0.0), 128);
        assert_eq!(encode_u8(1.0), 255);
        assert_eq!(encode_u8(2.0), 255);
        assert_eq!(encode_u8(-2.0), 0);

        // Values exactly representable with the 1/128 scaling.
        assert_eq!(encode_u8(0.5), 192);
        assert_eq!(encode_u8(-0.5), 64);

        // Non-finite inputs should never panic and should clamp deterministically.
        assert_eq!(encode_u8(f32::INFINITY), 255);
        assert_eq!(encode_u8(f32::NEG_INFINITY), 0);
        // `NaN` propagates through math ops and becomes 0 on `as u8` conversion.
        assert_eq!(encode_u8(f32::NAN), 0);
    }

    #[test]
    fn pcm_16bit_round_trip_and_clipping() {
        fn decode_i16(v: i16) -> f32 {
            decode_one_sample(&v.to_le_bytes(), 16)
        }
        fn encode_i16(sample: f32) -> i16 {
            let mut out = [0u8; 2];
            encode_one_sample(&mut out, 16, sample);
            i16::from_le_bytes(out)
        }

        let values: &[i16] = &[i16::MIN, -12345, -1, 0, 1, 12345, i16::MAX];
        for &v in values {
            let decoded = decode_i16(v);
            assert_f32_approx_eq(decoded, v as f32 / 32768.0, 1e-6);
            let round_tripped = encode_i16(decoded);
            assert_eq!(round_tripped, v, "round-trip failed for {v}");
        }

        // Clipping: values outside [-1.0, 1.0] saturate.
        assert_eq!(encode_i16(1.0), i16::MAX);
        assert_eq!(encode_i16(2.0), i16::MAX);
        assert_eq!(encode_i16(-1.0), i16::MIN);
        assert_eq!(encode_i16(-2.0), i16::MIN);

        // Non-finite inputs should never panic and should clamp deterministically.
        assert_eq!(encode_i16(f32::INFINITY), i16::MAX);
        assert_eq!(encode_i16(f32::NEG_INFINITY), i16::MIN);
        // `NaN` propagates through clamp/round and becomes 0 on `as i16` conversion.
        assert_eq!(encode_i16(f32::NAN), 0);
    }

    #[test]
    fn pcm_20bit_sign_extension_scaling_and_clipping() {
        fn decode_raw_20(v: i32) -> f32 {
            let raw = (v & 0x000F_FFFF) as u32; // low 20 bits, upper bits zero
            let bytes = raw.to_le_bytes();
            decode_one_sample(&bytes, 20)
        }
        fn encode_20(sample: f32) -> i32 {
            let mut out = [0u8; 4];
            encode_one_sample(&mut out, 20, sample);
            i32::from_le_bytes(out)
        }

        // Verify sign extension from the low 20 bits even when the stored upper bits are zero.
        assert_f32_approx_eq(decode_raw_20(-524_288), -1.0, 1e-6);
        assert_f32_approx_eq(decode_raw_20(-1), -1.0 / 524_288.0, 1e-6);
        assert_f32_approx_eq(decode_raw_20(0), 0.0, 1e-6);
        assert_f32_approx_eq(decode_raw_20(524_287), 524_287.0 / 524_288.0, 1e-6);

        // Upper bits may contain garbage; decoding should still use only the low 20 bits.
        let neg_one_with_garbage = 0xABC0_0000u32 | 0x000F_FFFFu32;
        assert_f32_approx_eq(
            decode_one_sample(&neg_one_with_garbage.to_le_bytes(), 20),
            -1.0 / 524_288.0,
            1e-6,
        );

        // Encode scaling and clipping.
        assert_eq!(encode_20(0.5), 262_144);
        assert_eq!(encode_20(-0.25), -131_072);
        assert_eq!(encode_20(1.0), 524_287);
        assert_eq!(encode_20(2.0), 524_287);
        assert_eq!(encode_20(-1.0), -524_288);
        assert_eq!(encode_20(-2.0), -524_288);

        // Non-finite inputs should never panic and should clamp deterministically.
        assert_eq!(encode_20(f32::INFINITY), 524_287);
        assert_eq!(encode_20(f32::NEG_INFINITY), -524_288);
        // `NaN` propagates through clamp/round and becomes 0 on `as i32` conversion.
        assert_eq!(encode_20(f32::NAN), 0);
    }

    #[test]
    fn pcm_20bit_selected_values_round_trip_through_f32() {
        fn decode_raw_20(v: i32) -> f32 {
            let raw = (v & 0x000F_FFFF) as u32;
            decode_one_sample(&raw.to_le_bytes(), 20)
        }
        fn encode_20(sample: f32) -> i32 {
            let mut out = [0u8; 4];
            encode_one_sample(&mut out, 20, sample);
            i32::from_le_bytes(out)
        }

        let values: &[i32] = &[-524_288, -12345, -1, 0, 1, 12345, 524_287];
        for &v in values {
            let decoded = decode_raw_20(v);
            let encoded = encode_20(decoded);
            assert_eq!(encoded, v, "20-bit round-trip failed for {v}");
        }
    }

    #[test]
    fn pcm_24bit_sign_extension_scaling_and_clipping() {
        fn decode_raw_24(v: i32) -> f32 {
            let raw = (v & 0x00FF_FFFF) as u32; // low 24 bits, upper bits zero
            let bytes = raw.to_le_bytes();
            decode_one_sample(&bytes, 24)
        }
        fn encode_24(sample: f32) -> i32 {
            let mut out = [0u8; 4];
            encode_one_sample(&mut out, 24, sample);
            i32::from_le_bytes(out)
        }

        assert_f32_approx_eq(decode_raw_24(-8_388_608), -1.0, 1e-6);
        assert_f32_approx_eq(decode_raw_24(-1), -1.0 / 8_388_608.0, 1e-6);
        assert_f32_approx_eq(decode_raw_24(0), 0.0, 1e-6);
        assert_f32_approx_eq(
            decode_raw_24(8_388_607),
            8_388_607.0 / 8_388_608.0,
            1e-6,
        );

        // Upper bits may contain garbage; decoding should still use only the low 24 bits.
        let neg_one_with_garbage = 0xAA00_0000u32 | 0x00FF_FFFFu32;
        assert_f32_approx_eq(
            decode_one_sample(&neg_one_with_garbage.to_le_bytes(), 24),
            -1.0 / 8_388_608.0,
            1e-6,
        );

        assert_eq!(encode_24(0.5), 4_194_304);
        assert_eq!(encode_24(-0.25), -2_097_152);
        assert_eq!(encode_24(1.0), 8_388_607);
        assert_eq!(encode_24(2.0), 8_388_607);
        assert_eq!(encode_24(-1.0), -8_388_608);
        assert_eq!(encode_24(-2.0), -8_388_608);

        // Non-finite inputs should never panic and should clamp deterministically.
        assert_eq!(encode_24(f32::INFINITY), 8_388_607);
        assert_eq!(encode_24(f32::NEG_INFINITY), -8_388_608);
        // `NaN` propagates through clamp/round and becomes 0 on `as i32` conversion.
        assert_eq!(encode_24(f32::NAN), 0);
    }

    #[test]
    fn pcm_24bit_selected_values_round_trip_through_f32() {
        fn decode_raw_24(v: i32) -> f32 {
            let raw = (v & 0x00FF_FFFF) as u32;
            decode_one_sample(&raw.to_le_bytes(), 24)
        }
        fn encode_24(sample: f32) -> i32 {
            let mut out = [0u8; 4];
            encode_one_sample(&mut out, 24, sample);
            i32::from_le_bytes(out)
        }

        let values: &[i32] = &[-8_388_608, -123456, -1, 0, 1, 123456, 8_388_607];
        for &v in values {
            let decoded = decode_raw_24(v);
            let encoded = encode_24(decoded);
            assert_eq!(encoded, v, "24-bit round-trip failed for {v}");
        }
    }

    #[test]
    fn pcm_32bit_scaling_and_clipping() {
        fn decode_i32(v: i32) -> f32 {
            decode_one_sample(&v.to_le_bytes(), 32)
        }
        fn encode_i32(sample: f32) -> i32 {
            let mut out = [0u8; 4];
            encode_one_sample(&mut out, 32, sample);
            i32::from_le_bytes(out)
        }

        assert_f32_approx_eq(decode_i32(i32::MIN), -1.0, 1e-6);
        assert_f32_approx_eq(decode_i32(0), 0.0, 1e-6);
        assert_f32_approx_eq(decode_i32(1 << 30), 0.5, 1e-6);
        assert_f32_approx_eq(decode_i32(-(1 << 30)), -0.5, 1e-6);
        // `i32::MAX` is not exactly representable as `f32` and rounds to 2^31, producing 1.0.
        assert_f32_approx_eq(decode_i32(i32::MAX), 1.0, 1e-6);

        assert_eq!(encode_i32(0.5), 1 << 30);
        assert_eq!(encode_i32(-0.5), -(1 << 30));
        assert_eq!(encode_i32(1.0), i32::MAX);
        assert_eq!(encode_i32(2.0), i32::MAX);
        assert_eq!(encode_i32(-1.0), i32::MIN);
        assert_eq!(encode_i32(-2.0), i32::MIN);

        // Non-finite inputs should never panic and should clamp deterministically.
        assert_eq!(encode_i32(f32::INFINITY), i32::MAX);
        assert_eq!(encode_i32(f32::NEG_INFINITY), i32::MIN);
        // `NaN` propagates through clamp/round and becomes 0 on `as i32` conversion.
        assert_eq!(encode_i32(f32::NAN), 0);
    }

    #[test]
    fn decode_pcm_to_stereo_f32_into_channel_mapping() {
        // Mono is duplicated to L/R.
        let fmt_mono = StreamFormat {
            sample_rate_hz: 48_000,
            bits_per_sample: 16,
            channels: 1,
        };
        let input = 1234i16.to_le_bytes();
        let mut out = Vec::new();
        decode_pcm_to_stereo_f32_into(&input, fmt_mono, &mut out);
        assert_eq!(out.len(), 1);
        let expected = 1234.0 / 32768.0;
        assert_f32_approx_eq(out[0][0], expected, 1e-6);
        assert_f32_approx_eq(out[0][1], expected, 1e-6);

        // >=2 channels: first two channels are used.
        let fmt_4ch = StreamFormat {
            sample_rate_hz: 48_000,
            bits_per_sample: 16,
            channels: 4,
        };
        let frame = [
            1000i16.to_le_bytes(),
            (-1000i16).to_le_bytes(),
            2222i16.to_le_bytes(),
            (-2222i16).to_le_bytes(),
        ]
        .concat();
        decode_pcm_to_stereo_f32_into(&frame, fmt_4ch, &mut out);
        assert_eq!(out.len(), 1);
        assert_f32_approx_eq(out[0][0], 1000.0 / 32768.0, 1e-6);
        assert_f32_approx_eq(out[0][1], -1000.0 / 32768.0, 1e-6);
    }

    #[test]
    fn decode_pcm_to_stereo_f32_ignores_partial_trailing_frame() {
        // 16-bit stereo => 4 bytes per frame. Provide 1 full frame + 2 extra bytes.
        let fmt = StreamFormat {
            sample_rate_hz: 48_000,
            bits_per_sample: 16,
            channels: 2,
        };
        let mut input = Vec::new();
        input.extend_from_slice(&1000i16.to_le_bytes());
        input.extend_from_slice(&(-1000i16).to_le_bytes());
        input.extend_from_slice(&[0xAA, 0xBB]); // trailing partial frame

        let out = decode_pcm_to_stereo_f32(&input, fmt);
        assert_eq!(out.len(), 1);
        assert_f32_approx_eq(out[0][0], 1000.0 / 32768.0, 1e-6);
        assert_f32_approx_eq(out[0][1], -1000.0 / 32768.0, 1e-6);
    }

    #[test]
    fn encode_mono_f32_to_pcm_into_channel_mapping() {
        // channels==1 encodes mono frames.
        let fmt_mono = StreamFormat {
            sample_rate_hz: 48_000,
            bits_per_sample: 16,
            channels: 1,
        };
        let mut out = Vec::new();
        encode_mono_f32_to_pcm_into(&[0.5, -0.5], fmt_mono, &mut out);
        assert_eq!(out.len(), 2 * 2);
        let a = i16::from_le_bytes([out[0], out[1]]);
        let b = i16::from_le_bytes([out[2], out[3]]);
        assert_eq!(a, 16384);
        assert_eq!(b, -16384);

        // channels>=2 duplicates into the first two channels, remaining channels are silence.
        let fmt_4ch = StreamFormat {
            sample_rate_hz: 48_000,
            bits_per_sample: 16,
            channels: 4,
        };
        encode_mono_f32_to_pcm_into(&[0.25], fmt_4ch, &mut out);
        assert_eq!(out.len(), 1 * 2 * 4);
        let ch0 = i16::from_le_bytes([out[0], out[1]]);
        let ch1 = i16::from_le_bytes([out[2], out[3]]);
        let ch2 = i16::from_le_bytes([out[4], out[5]]);
        let ch3 = i16::from_le_bytes([out[6], out[7]]);
        assert_eq!(ch0, 8192);
        assert_eq!(ch1, 8192);
        assert_eq!(ch2, 0);
        assert_eq!(ch3, 0);
    }

    #[test]
    fn encode_then_decode_round_trips_for_common_formats() {
        // Choose values that are exactly representable with all current scaling factors.
        let input: &[f32] = &[0.0, 0.5, -0.5, 0.25, -0.25];

        let fmts = [
            StreamFormat {
                sample_rate_hz: 48_000,
                bits_per_sample: 8,
                channels: 1,
            },
            StreamFormat {
                sample_rate_hz: 48_000,
                bits_per_sample: 16,
                channels: 2,
            },
            StreamFormat {
                sample_rate_hz: 48_000,
                bits_per_sample: 20,
                channels: 4,
            },
            StreamFormat {
                sample_rate_hz: 48_000,
                bits_per_sample: 24,
                channels: 1,
            },
            StreamFormat {
                sample_rate_hz: 48_000,
                bits_per_sample: 32,
                channels: 6,
            },
        ];

        for fmt in fmts {
            let pcm = encode_mono_f32_to_pcm(input, fmt);
            // Ensure channel padding is silence for multi-channel formats.
            if fmt.channels >= 3 {
                let bps = fmt.bytes_per_sample();
                let bpf = fmt.bytes_per_frame();
                let frame0 = &pcm[0..bpf];
                // channels 2.. are silence.
                for ch in 2..fmt.channels as usize {
                    let off = ch * bps;
                    assert!(frame0[off..off + bps].iter().all(|&b| b == 0));
                }
            }

            let decoded = decode_pcm_to_stereo_f32(&pcm, fmt);
            assert_eq!(decoded.len(), input.len());
            for (frame, &mono) in decoded.iter().zip(input) {
                assert_f32_approx_eq(frame[0], mono, 1e-6);
                assert_f32_approx_eq(frame[1], mono, 1e-6);
            }
        }
    }

    #[test]
    fn decode_pcm_to_stereo_f32_handles_8bit_and_16bit_frames() {
        // 8-bit unsigned PCM is biased by 128.
        let fmt = StreamFormat {
            sample_rate_hz: 48_000,
            bits_per_sample: 8,
            channels: 2,
        };
        let out = decode_pcm_to_stereo_f32(&[0x00, 0xFF], fmt);
        assert_eq!(out.len(), 1);
        assert_f32_approx_eq(out[0][0], -1.0, 1e-6);
        assert_f32_approx_eq(out[0][1], 127.0 / 128.0, 1e-6);

        let fmt = StreamFormat {
            sample_rate_hz: 48_000,
            bits_per_sample: 16,
            channels: 2,
        };
        let mut input = Vec::new();
        input.extend_from_slice(&i16::MIN.to_le_bytes());
        input.extend_from_slice(&i16::MAX.to_le_bytes());
        let out = decode_pcm_to_stereo_f32(&input, fmt);
        assert_eq!(out.len(), 1);
        assert_f32_approx_eq(out[0][0], -1.0, 1e-6);
        assert_f32_approx_eq(out[0][1], i16::MAX as f32 / 32768.0, 1e-6);
    }

    #[test]
    fn decode_pcm_to_stereo_f32_handles_20_24_32bit_frames() {
        // These tests exercise the public framing/mixing path, not just `decode_one_sample`.
        let cases: &[(u8, i32, i32, f32, f32)] = &[
            // bits, left_raw, right_raw, left_f32, right_f32
            (20, 262_144, -262_144, 0.5, -0.5),
            (24, 4_194_304, -4_194_304, 0.5, -0.5),
            (32, 1 << 30, -(1 << 30), 0.5, -0.5),
        ];
        for &(bits, l_raw, r_raw, l_f32, r_f32) in cases {
            let fmt = StreamFormat {
                sample_rate_hz: 48_000,
                bits_per_sample: bits,
                channels: 2,
            };
            let mut input = Vec::new();
            input.extend_from_slice(&l_raw.to_le_bytes());
            input.extend_from_slice(&r_raw.to_le_bytes());
            let out = decode_pcm_to_stereo_f32(&input, fmt);
            assert_eq!(out.len(), 1);
            assert_f32_approx_eq(out[0][0], l_f32, 1e-6);
            assert_f32_approx_eq(out[0][1], r_f32, 1e-6);
        }
    }

    #[test]
    fn encode_mono_f32_to_pcm_writes_exact_bytes_for_8bit_unsigned() {
        let fmt = StreamFormat {
            sample_rate_hz: 48_000,
            bits_per_sample: 8,
            channels: 4,
        };
        let input: &[f32] = &[
            -1.0,
            -0.5,
            0.0,
            0.5,
            1.0,
            1.1,
            -1.1,
            f32::NAN,
            f32::INFINITY,
            f32::NEG_INFINITY,
        ];
        let out = encode_mono_f32_to_pcm(input, fmt);

        // -1.0 -> 0, 0.0 -> 128, 1.0 -> 255.
        // 0.5 -> 192, -0.5 -> 64.
        // Values outside [-1, 1] are clipped; NaN casts to 0 after going through math.
        let mut expected = Vec::new();
        for &v in &[0u8, 64, 128, 192, 255, 255, 0, 0, 255, 0] {
            // Duplicated into the first two channels.
            expected.extend_from_slice(&[v, v]);
            // Remaining channels are silence: 0.0 -> 128 in unsigned PCM.
            expected.extend_from_slice(&[128u8, 128u8]);
        }
        assert_eq!(out, expected);
    }

    #[test]
    fn encode_mono_f32_to_pcm_writes_exact_bytes_for_16bit_clip_and_non_finite() {
        let fmt = StreamFormat {
            sample_rate_hz: 48_000,
            bits_per_sample: 16,
            channels: 2,
        };
        let input: &[f32] = &[
            -1.0,
            0.5,
            -0.5,
            0.0,
            1.0,
            1.1,
            -1.1,
            f32::NAN,
            f32::INFINITY,
            f32::NEG_INFINITY,
        ];
        let out = encode_mono_f32_to_pcm(input, fmt);

        let mut expected = Vec::new();
        for &v in &[
            i16::MIN,
            16384,
            -16384,
            0,
            i16::MAX,
            i16::MAX,
            i16::MIN,
            0,
            i16::MAX,
            i16::MIN,
        ] {
            expected.extend_from_slice(&v.to_le_bytes());
            expected.extend_from_slice(&v.to_le_bytes());
        }
        assert_eq!(out, expected);
    }

    #[test]
    fn encode_mono_f32_to_pcm_writes_exact_bytes_for_20bit() {
        let fmt = StreamFormat {
            sample_rate_hz: 48_000,
            bits_per_sample: 20,
            channels: 4,
        };
        let input: &[f32] = &[
            -1.0,
            0.5,
            -0.5,
            0.0,
            1.0,
            1.1,
            -1.1,
            f32::NAN,
            f32::INFINITY,
            f32::NEG_INFINITY,
        ];
        let out = encode_mono_f32_to_pcm(input, fmt);

        let mut expected = Vec::new();
        for &v in &[
            -524_288i32,
            262_144,
            -262_144,
            0,
            524_287,
            524_287,
            -524_288,
            0,
            524_287,
            -524_288,
        ] {
            // Duplicated into the first two channels.
            expected.extend_from_slice(&v.to_le_bytes());
            expected.extend_from_slice(&v.to_le_bytes());
            // Remaining channels are silence.
            expected.extend_from_slice(&0i32.to_le_bytes());
            expected.extend_from_slice(&0i32.to_le_bytes());
        }
        assert_eq!(out, expected);
    }

    #[test]
    fn encode_mono_f32_to_pcm_writes_exact_bytes_for_24bit_clip_and_non_finite() {
        let fmt = StreamFormat {
            sample_rate_hz: 48_000,
            bits_per_sample: 24,
            channels: 4,
        };
        let input: &[f32] = &[
            -1.0,
            0.5,
            -0.5,
            0.0,
            1.0,
            1.1,
            -1.1,
            f32::NAN,
            f32::INFINITY,
            f32::NEG_INFINITY,
        ];
        let out = encode_mono_f32_to_pcm(input, fmt);

        let mut expected = Vec::new();
        for &v in &[
            -8_388_608i32,
            4_194_304,
            -4_194_304,
            0,
            8_388_607,
            8_388_607,
            -8_388_608,
            0,
            8_388_607,
            -8_388_608,
        ] {
            expected.extend_from_slice(&v.to_le_bytes());
            expected.extend_from_slice(&v.to_le_bytes());
            expected.extend_from_slice(&0i32.to_le_bytes());
            expected.extend_from_slice(&0i32.to_le_bytes());
        }
        assert_eq!(out, expected);
    }

    #[test]
    fn encode_mono_f32_to_pcm_writes_exact_bytes_for_32bit_clip_and_non_finite() {
        let fmt = StreamFormat {
            sample_rate_hz: 48_000,
            bits_per_sample: 32,
            channels: 6,
        };
        let input: &[f32] = &[
            -1.0,
            0.5,
            -0.5,
            0.0,
            1.0,
            1.1,
            -1.1,
            f32::NAN,
            f32::INFINITY,
            f32::NEG_INFINITY,
        ];
        let out = encode_mono_f32_to_pcm(input, fmt);

        let mut expected = Vec::new();
        for &v in &[
            i32::MIN,
            1 << 30,
            -(1 << 30),
            0,
            i32::MAX,
            i32::MAX,
            i32::MIN,
            0,
            i32::MAX,
            i32::MIN,
        ] {
            expected.extend_from_slice(&v.to_le_bytes());
            expected.extend_from_slice(&v.to_le_bytes());
            expected.extend_from_slice(&0i32.to_le_bytes());
            expected.extend_from_slice(&0i32.to_le_bytes());
            expected.extend_from_slice(&0i32.to_le_bytes());
            expected.extend_from_slice(&0i32.to_le_bytes());
        }
        assert_eq!(out, expected);
    }

    #[test]
    fn resampler_upsamples_monotonic() {
        let mut res = LinearResampler::new(44_100, 48_000);
        // Push a ramp.
        let src: Vec<[f32; 2]> = (0..1000)
            .map(|i| {
                let v = i as f32 / 1000.0;
                [v, v]
            })
            .collect();
        res.push_source_frames(&src);

        // Ask for some output; just ensure we get something non-empty and in range.
        let out = res.produce_interleaved_stereo(128);
        assert_eq!(out.len() % 2, 0);
        assert!(!out.is_empty());
        for s in out {
            assert!((0.0..=1.0).contains(&s));
        }
    }
}
