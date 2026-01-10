use core::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PcmSampleFormat {
    /// Unsigned 8-bit PCM (common for WAV).
    U8,
    /// Signed 8-bit PCM.
    I8,
    /// Signed 16-bit PCM, little-endian.
    I16,
    /// Signed 20-bit PCM stored in a 32-bit little-endian word (valid bits are the low 20).
    I20In32,
    /// Signed 24-bit PCM stored in a 32-bit little-endian word (valid bits are the low 24).
    I24In32,
    /// Signed 20-bit PCM stored in 3 bytes little-endian (valid bits are the low 20).
    I20In3,
    /// Signed 24-bit PCM stored in 3 bytes little-endian.
    I24In3,
    /// Signed 32-bit PCM, little-endian.
    I32,
    /// 32-bit float PCM, little-endian.
    F32,
}

impl PcmSampleFormat {
    #[inline]
    pub const fn bytes_per_sample(self) -> usize {
        match self {
            Self::U8 | Self::I8 => 1,
            Self::I16 => 2,
            Self::I20In3 | Self::I24In3 => 3,
            Self::I20In32 | Self::I24In32 | Self::I32 | Self::F32 => 4,
        }
    }

    #[inline]
    pub const fn bits_per_sample(self) -> u8 {
        match self {
            Self::U8 | Self::I8 => 8,
            Self::I16 => 16,
            Self::I20In32 | Self::I20In3 => 20,
            Self::I24In32 | Self::I24In3 => 24,
            Self::I32 => 32,
            Self::F32 => 32,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PcmSpec {
    pub format: PcmSampleFormat,
    pub channels: usize,
    pub sample_rate: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PcmError {
    InvalidChannels,
    InputLengthNotAligned { expected_multiple: usize },
}

impl fmt::Display for PcmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidChannels => write!(f, "invalid channel count"),
            Self::InputLengthNotAligned { expected_multiple } => write!(
                f,
                "PCM input length is not a multiple of {expected_multiple} bytes"
            ),
        }
    }
}

impl std::error::Error for PcmError {}

#[inline]
fn ensure_capacity_and_len(out: &mut Vec<f32>, len: usize) {
    out.clear();
    if out.capacity() < len {
        out.reserve(len - out.capacity());
    }
    // SAFETY: we fully initialise all elements below.
    unsafe {
        out.set_len(len);
    }
}

/// Decode interleaved PCM bytes into interleaved `f32` samples in `[-1.0, 1.0)`.
///
/// The output buffer is reused (no per-call allocations as long as capacity is sufficient).
pub fn decode_interleaved_to_f32(
    input: &[u8],
    format: PcmSampleFormat,
    channels: usize,
    out: &mut Vec<f32>,
) -> Result<(), PcmError> {
    if channels == 0 {
        return Err(PcmError::InvalidChannels);
    }

    let bps = format.bytes_per_sample();
    let bpf = bps * channels;
    if input.len() % bpf != 0 {
        return Err(PcmError::InputLengthNotAligned {
            expected_multiple: bpf,
        });
    }

    let frames = input.len() / bpf;
    let total_samples = frames * channels;
    ensure_capacity_and_len(out, total_samples);

    match format {
        PcmSampleFormat::U8 => {
            // Map [0, 255] -> [-1.0, ~1.0)
            for (o, &b) in out.iter_mut().zip(input.iter()) {
                *o = (b as f32 - 128.0) * (1.0 / 128.0);
            }
        }
        PcmSampleFormat::I8 => {
            for (o, &b) in out.iter_mut().zip(input.iter()) {
                *o = (b as i8) as f32 * (1.0 / 128.0);
            }
        }
        PcmSampleFormat::I16 => {
            let mut i = 0;
            for chunk in input.chunks_exact(2) {
                let s = i16::from_le_bytes([chunk[0], chunk[1]]);
                out[i] = s as f32 * (1.0 / 32768.0);
                i += 1;
            }
        }
        PcmSampleFormat::I20In32 => {
            let mut i = 0;
            for chunk in input.chunks_exact(4) {
                let raw = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                // Sign extend from bit 19.
                let s = ((raw << 12) as i32) >> 12;
                out[i] = s as f32 * (1.0 / 524288.0);
                i += 1;
            }
        }
        PcmSampleFormat::I24In32 => {
            let mut i = 0;
            for chunk in input.chunks_exact(4) {
                let raw = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                // Sign extend from bit 23.
                let s = ((raw << 8) as i32) >> 8;
                out[i] = s as f32 * (1.0 / 8_388_608.0);
                i += 1;
            }
        }
        PcmSampleFormat::I20In3 => {
            let mut i = 0;
            for chunk in input.chunks_exact(3) {
                let raw = (chunk[0] as u32) | ((chunk[1] as u32) << 8) | ((chunk[2] as u32) << 16);
                // Sign extend from bit 19 (low 20 bits).
                let s = ((raw << 12) as i32) >> 12;
                out[i] = s as f32 * (1.0 / 524288.0);
                i += 1;
            }
        }
        PcmSampleFormat::I24In3 => {
            let mut i = 0;
            for chunk in input.chunks_exact(3) {
                let raw = (chunk[0] as u32) | ((chunk[1] as u32) << 8) | ((chunk[2] as u32) << 16);
                // Sign extend from bit 23.
                let s = ((raw << 8) as i32) >> 8;
                out[i] = s as f32 * (1.0 / 8_388_608.0);
                i += 1;
            }
        }
        PcmSampleFormat::I32 => {
            let mut i = 0;
            for chunk in input.chunks_exact(4) {
                let s = i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                out[i] = s as f32 * (1.0 / 2_147_483_648.0);
                i += 1;
            }
        }
        PcmSampleFormat::F32 => {
            let mut i = 0;
            for chunk in input.chunks_exact(4) {
                out[i] = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                i += 1;
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_u8_mono() {
        let input = [0u8, 128u8, 255u8];
        let mut out = Vec::new();
        decode_interleaved_to_f32(&input, PcmSampleFormat::U8, 1, &mut out).unwrap();
        assert_eq!(out.len(), 3);
        assert!((out[0] - (-1.0)).abs() < 1e-6);
        assert!((out[1] - 0.0).abs() < 1e-6);
        assert!((out[2] - 0.992_187_5).abs() < 1e-6);
    }

    #[test]
    fn decode_i16_mono() {
        // -32768, 0, 32767
        let input = [0x00, 0x80, 0x00, 0x00, 0xFF, 0x7F];
        let mut out = Vec::new();
        decode_interleaved_to_f32(&input, PcmSampleFormat::I16, 1, &mut out).unwrap();
        assert_eq!(out.len(), 3);
        assert!((out[0] - (-1.0)).abs() < 1e-6);
        assert!((out[1] - 0.0).abs() < 1e-6);
        assert!((out[2] - 0.999_969_5).abs() < 1e-6);
    }

    #[test]
    fn decode_i20_in32() {
        // -2^19, 0, 2^19-1
        let input = [
            0x00, 0x00, 0x08, 0x00, // 0x0008_0000 (sign bit 19 set) => -524288
            0x00, 0x00, 0x00, 0x00, // 0
            0xFF, 0xFF, 0x07, 0x00, // 0x0007_FFFF => 524287
        ];
        let mut out = Vec::new();
        decode_interleaved_to_f32(&input, PcmSampleFormat::I20In32, 1, &mut out).unwrap();
        assert_eq!(out.len(), 3);
        assert!((out[0] - (-1.0)).abs() < 1e-6);
        assert!((out[1] - 0.0).abs() < 1e-6);
        assert!((out[2] - (524_287.0 / 524_288.0)).abs() < 1e-6);
    }

    #[test]
    fn decode_i24_in32() {
        // -2^23, 0, 2^23-1
        let input = [
            0x00, 0x00, 0x80, 0x00, // 0x0080_0000 => -8388608
            0x00, 0x00, 0x00, 0x00, // 0
            0xFF, 0xFF, 0x7F, 0x00, // 0x007F_FFFF => 8388607
        ];
        let mut out = Vec::new();
        decode_interleaved_to_f32(&input, PcmSampleFormat::I24In32, 1, &mut out).unwrap();
        assert_eq!(out.len(), 3);
        assert!((out[0] - (-1.0)).abs() < 1e-6);
        assert!((out[1] - 0.0).abs() < 1e-6);
        assert!((out[2] - (8_388_607.0 / 8_388_608.0)).abs() < 1e-6);
    }

    #[test]
    fn decode_f32() {
        let input = [
            0x00, 0x00, 0x00, 0xBF, // -0.5
            0x00, 0x00, 0x00, 0x00, // 0.0
            0x00, 0x00, 0x00, 0x3F, // 0.5
        ];
        let mut out = Vec::new();
        decode_interleaved_to_f32(&input, PcmSampleFormat::F32, 1, &mut out).unwrap();
        assert_eq!(out, vec![-0.5, 0.0, 0.5]);
    }
}
