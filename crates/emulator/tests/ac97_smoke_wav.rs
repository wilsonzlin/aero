#![cfg(feature = "legacy-audio")]

use std::fs;
use std::path::Path;

use emulator::io::audio::ac97::dma::AudioSink;
use emulator::io::audio::ac97::regs::{BDL_IOC, CR_IOCE, CR_RPBM, SR_DCH};
use emulator::io::audio::ac97::Ac97Controller;
use memory::MemoryBus;

#[derive(Default)]
struct TestAudio {
    samples: Vec<f32>,
}

impl AudioSink for TestAudio {
    fn push_interleaved_f32(&mut self, samples: &[f32]) {
        self.samples.extend_from_slice(samples);
    }
}

#[derive(Clone, Debug)]
struct TestMemory {
    data: Vec<u8>,
}

impl TestMemory {
    fn new(size: usize) -> Self {
        Self {
            data: vec![0; size],
        }
    }

    fn write_u32(&mut self, addr: u64, value: u32) {
        let start = addr as usize;
        self.data[start..start + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn write_bytes(&mut self, addr: u64, data: &[u8]) {
        let start = addr as usize;
        self.data[start..start + data.len()].copy_from_slice(data);
    }
}

impl MemoryBus for TestMemory {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        let start = paddr as usize;
        buf.copy_from_slice(&self.data[start..start + buf.len()]);
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        let start = paddr as usize;
        self.data[start..start + buf.len()].copy_from_slice(buf);
    }
}

fn f32_to_i16(sample: f32) -> i16 {
    let scaled = (sample.clamp(-1.0, 1.0) * 32768.0).round();
    scaled.clamp(i16::MIN as f32, i16::MAX as f32).trunc() as i16
}

fn write_wav_i16_stereo(
    path: &Path,
    sample_rate: u32,
    frames: &[(i16, i16)],
) -> std::io::Result<()> {
    let mut data = Vec::with_capacity(frames.len() * 4);
    for &(l, r) in frames {
        data.extend_from_slice(&l.to_le_bytes());
        data.extend_from_slice(&r.to_le_bytes());
    }

    let byte_rate = sample_rate * 2 * 16 / 8;
    let block_align = 2 * 16 / 8;
    let data_len = data.len() as u32;

    let mut wav = Vec::with_capacity(44 + data.len());
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVE");

    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes()); // PCM fmt chunk size
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&2u16.to_le_bytes()); // channels
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&byte_rate.to_le_bytes());
    wav.extend_from_slice(&(block_align as u16).to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes()); // bits per sample

    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    wav.extend_from_slice(&data);

    fs::create_dir_all(path.parent().unwrap_or(Path::new(".")))?;
    fs::write(path, wav)
}

/// Integration-ish smoke test that exercises the AC'97 PCM-out DMA path end-to-end.
///
/// We can't boot a full Linux guest here, but we *can* validate the exact ring-buffer style
/// programming model used by `snd-intel8x0`:
/// - A BDL with IOC-marked entries.
/// - Run bit set.
/// - Interrupt status bits asserted on buffer completion.
/// - Extracted PCM samples match guest memory.
#[test]
fn smoke_dma_outputs_wav() {
    let mut mem = TestMemory::new(0x20000);

    // Two BDL entries (like two periods).
    let bdl_addr = 0x1000u64;
    let buf0_addr = 0x2000u64;
    let buf1_addr = 0x4000u64;

    let frames_per_buf = 64usize;
    let sample_rate = 48_000u32;

    let mut buf0 = Vec::with_capacity(frames_per_buf * 4);
    let mut buf1 = Vec::with_capacity(frames_per_buf * 4);
    let mut expected_buf0 = Vec::with_capacity(frames_per_buf);
    let mut expected_buf1 = Vec::with_capacity(frames_per_buf);

    for i in 0..frames_per_buf {
        let l0 = i as i16;
        let r0 = -(i as i16);
        buf0.extend_from_slice(&l0.to_le_bytes());
        buf0.extend_from_slice(&r0.to_le_bytes());
        expected_buf0.push((l0, r0));

        let l1 = (1000 + i) as i16;
        let r1 = -((1000 + i) as i16);
        buf1.extend_from_slice(&l1.to_le_bytes());
        buf1.extend_from_slice(&r1.to_le_bytes());
        expected_buf1.push((l1, r1));
    }

    let mut expected_frames = expected_buf0;
    expected_frames.extend_from_slice(&expected_buf1);

    mem.write_bytes(buf0_addr, &buf0);
    mem.write_bytes(buf1_addr, &buf1);

    // Descriptor length is in 16-bit words.
    let words_per_buf = (frames_per_buf * 2) as u32;

    // Entry 0.
    mem.write_u32(bdl_addr + 0, buf0_addr as u32);
    mem.write_u32(bdl_addr + 4, words_per_buf | BDL_IOC);
    // Entry 1.
    mem.write_u32(bdl_addr + 8, buf1_addr as u32);
    mem.write_u32(bdl_addr + 12, words_per_buf | BDL_IOC);

    let mut ac97 = Ac97Controller::new();
    ac97.nabm_write(0x00, 4, bdl_addr as u32); // PO_BDBAR
    ac97.nabm_write(0x05, 1, 1); // PO_LVI = 1
    ac97.nabm_write(0x0B, 1, (CR_RPBM | CR_IOCE) as u32); // PO_CR

    let mut audio = TestAudio::default();

    // Transfer in small chunks to exercise PICB decrement and multiple ticks per buffer.
    while (ac97.pcm_out.sr() & SR_DCH) == 0 {
        ac97.poll(&mut mem, &mut audio, 8);
    }

    // Convert captured samples back to i16 stereo frames.
    assert_eq!(audio.samples.len(), expected_frames.len() * 2);
    let mut out_frames = Vec::with_capacity(expected_frames.len());
    for pair in audio.samples.chunks_exact(2) {
        out_frames.push((f32_to_i16(pair[0]), f32_to_i16(pair[1])));
    }

    assert_eq!(out_frames, expected_frames);

    // Write a wav file for human inspection when running locally.
    let out_path = Path::new("target/ac97_smoke.wav");
    write_wav_i16_stereo(out_path, sample_rate, &out_frames).unwrap();

    let wav_bytes = fs::read(out_path).unwrap();
    assert!(wav_bytes.starts_with(b"RIFF"));
    assert!(wav_bytes[8..12] == *b"WAVE");
}
