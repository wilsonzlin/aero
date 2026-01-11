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
        let base = if (fmt & (1 << 14)) != 0 { 44_100 } else { 48_000 };
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
    let bytes_per_frame = fmt.bytes_per_frame();
    if bytes_per_frame == 0 {
        return Vec::new();
    }
    let frames = input.len() / bytes_per_frame;
    let mut out = Vec::with_capacity(frames);

    for frame in 0..frames {
        let frame_off = frame * bytes_per_frame;
        let chan = |ch: u8| -> f32 {
            let ch = ch as usize;
            let off = frame_off + ch * fmt.bytes_per_sample();
            decode_one_sample(&input[off..off + fmt.bytes_per_sample()], fmt.bits_per_sample)
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

    out
}

/// Encode mono `f32` samples into interleaved PCM bytes as described by `fmt`.
///
/// Channel mapping:
/// - 1 channel: mono
/// - 2+ channels: the mono signal is duplicated into the first two channels and
///   remaining channels are filled with silence.
pub fn encode_mono_f32_to_pcm(input: &[f32], fmt: StreamFormat) -> Vec<u8> {
    let bytes_per_frame = fmt.bytes_per_frame();
    if bytes_per_frame == 0 {
        return Vec::new();
    }

    let mut out = vec![0u8; input.len() * bytes_per_frame];
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

    out
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
        for _ in 0..dst_frames {
            let idx = self.src_pos.floor() as usize;
            let frac = self.src_pos - idx as f64;
            let a = match self.src.get(idx) {
                Some(v) => *v,
                None => break,
            };
            let (l, r) = if frac.abs() <= 1e-12 {
                (a[0], a[1])
            } else {
                let b = match self.src.get(idx + 1) {
                    Some(v) => *v,
                    None => break,
                };
                (
                    lerp(a[0], b[0], frac as f32),
                    lerp(a[1], b[1], frac as f32),
                )
            };
            out.push(l);
            out.push(r);
            self.src_pos += self.step_src_per_dst;
            self.drop_consumed();
        }
        out
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

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

#[cfg(test)]
mod tests {
    use super::*;

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
