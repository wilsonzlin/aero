#![cfg(feature = "legacy-audio")]

use emulator::io::audio::dsp::pcm::{PcmSampleFormat, PcmSpec};
use emulator::io::audio::dsp::resample::ResamplerKind;
use emulator::io::audio::dsp::StreamProcessor;

fn build_test_wav() -> Vec<u8> {
    // 16-bit PCM, stereo, 44.1 kHz.
    let sample_rate = 44_100u32;
    let channels = 2u16;
    let bits_per_sample = 16u16;
    let frames = 256u32;

    let block_align = channels * (bits_per_sample / 8);
    let byte_rate = sample_rate * block_align as u32;
    let data_size = frames * block_align as u32;

    let mut data = Vec::with_capacity(data_size as usize);
    for i in 0..frames {
        // Deterministic (non-sine) pattern to keep the golden stable across platforms.
        let l = ((i as i32 * 257 + 12345) % 65536 - 32768) as i16;
        let r = (-l) as i16;
        data.extend_from_slice(&l.to_le_bytes());
        data.extend_from_slice(&r.to_le_bytes());
    }

    let fmt_chunk_size = 16u32;
    let riff_size = 4 + (8 + fmt_chunk_size) + (8 + data_size);

    let mut wav = Vec::with_capacity((riff_size + 8) as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&riff_size.to_le_bytes());
    wav.extend_from_slice(b"WAVE");

    // fmt chunk
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&fmt_chunk_size.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&channels.to_le_bytes());
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&byte_rate.to_le_bytes());
    wav.extend_from_slice(&block_align.to_le_bytes());
    wav.extend_from_slice(&bits_per_sample.to_le_bytes());

    // data chunk
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_size.to_le_bytes());
    wav.extend_from_slice(&data);

    wav
}

fn parse_wav(data: &[u8]) -> (PcmSpec, &[u8]) {
    assert!(data.starts_with(b"RIFF"));
    assert!(&data[8..12] == b"WAVE");

    let mut offset = 12;
    let mut fmt: Option<(u16, u16, u32, u16)> = None;
    let mut pcm_data: Option<&[u8]> = None;

    while offset + 8 <= data.len() {
        let id = &data[offset..offset + 4];
        let size = u32::from_le_bytes(data[offset + 4..offset + 8].try_into().unwrap()) as usize;
        offset += 8;
        if offset + size > data.len() {
            break;
        }

        match id {
            b"fmt " => {
                let audio_format = u16::from_le_bytes(data[offset..offset + 2].try_into().unwrap());
                let channels = u16::from_le_bytes(data[offset + 2..offset + 4].try_into().unwrap());
                let sample_rate =
                    u32::from_le_bytes(data[offset + 4..offset + 8].try_into().unwrap());
                let bits_per_sample =
                    u16::from_le_bytes(data[offset + 14..offset + 16].try_into().unwrap());
                fmt = Some((audio_format, channels, sample_rate, bits_per_sample));
            }
            b"data" => {
                pcm_data = Some(&data[offset..offset + size]);
            }
            _ => {}
        }

        // Chunks are word-aligned.
        offset += size + (size & 1);
    }

    let (audio_format, channels, sample_rate, bits_per_sample) = fmt.expect("missing fmt chunk");
    let pcm_data = pcm_data.expect("missing data chunk");

    let format = match (audio_format, bits_per_sample) {
        (1, 16) => PcmSampleFormat::I16,
        (1, 8) => PcmSampleFormat::U8,
        (1, 24) => PcmSampleFormat::I24In3,
        (3, 32) => PcmSampleFormat::F32,
        _ => panic!("unsupported test wav format {audio_format}/{bits_per_sample}"),
    };

    (
        PcmSpec {
            format,
            channels: channels as usize,
            sample_rate,
        },
        pcm_data,
    )
}

fn hash_f32(samples: &[f32]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325; // FNV-1a
    for &s in samples {
        h ^= s.to_bits() as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

#[test]
fn golden_wav_to_float32_checksum() {
    let wav = build_test_wav();
    let (spec, pcm_bytes) = parse_wav(&wav);

    let mut proc = StreamProcessor::new(spec, 48_000, 2, ResamplerKind::Linear).unwrap();

    let mut out = Vec::new();
    let mut tail = Vec::new();
    proc.process(pcm_bytes, &mut out).unwrap();
    proc.flush(&mut tail).unwrap();
    out.extend_from_slice(&tail);

    // This checksum is the contract: changes here should be intentional and justified.
    assert_eq!(hash_f32(&out), 0xadf6_a4ce_b91e_6b2f);
}
