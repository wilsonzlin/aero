#![cfg(feature = "legacy-audio")]

use emulator::io::audio::dsp::pcm::{PcmSampleFormat, PcmSpec};
use emulator::io::audio::dsp::resample::ResamplerKind;
use emulator::io::audio::dsp::StreamProcessor;
use emulator::io::audio::hda::regs::*;
use emulator::io::audio::hda::HdaController;
use memory::{Bus, MemoryBus};

fn build_test_pcm_44k1_stereo_i16(frames: usize) -> Vec<u8> {
    let mut data = Vec::with_capacity(frames * 4);
    for i in 0..frames {
        // Deterministic (non-sine) pattern to keep the golden stable across platforms.
        let l = ((i as i32 * 257 + 12345) % 65536 - 32768) as i16;
        let r = -l;
        data.extend_from_slice(&l.to_le_bytes());
        data.extend_from_slice(&r.to_le_bytes());
    }
    data
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
fn hda_stream_dma_to_dsp_stream_processor_checksum() {
    let frames = 256usize;
    let pcm_bytes = build_test_pcm_44k1_stereo_i16(frames);
    let pcm_len = pcm_bytes.len();

    let mut mem = Bus::new(0x20_000);
    let mut hda = HdaController::new();

    // Leave controller reset so `poll()` performs DMA.
    hda.mmio_write(HDA_GCTL, 4, GCTL_CRST as u64);

    // Configure a single-entry BDL for SD0.
    let bdl_base = 0x4000u64;
    let buf0 = 0x5000u64;

    mem.write_u64(bdl_base, buf0);
    mem.write_u32(bdl_base + 8, pcm_len as u32);
    mem.write_u32(bdl_base + 12, 0); // no IOC needed for this smoke test

    mem.write_physical(buf0, &pcm_bytes);

    // Stream format: 44.1kHz base, 16-bit, 2 channels.
    let fmt_44k1_stereo_16 = 0x4011u16;

    hda.mmio_write(HDA_SD0BDPL, 4, bdl_base);
    hda.mmio_write(HDA_SD0BDPU, 4, 0);
    hda.mmio_write(HDA_SD0LVI, 2, 0); // one BDL entry
    hda.mmio_write(HDA_SD0CBL, 4, pcm_len as u64);
    hda.mmio_write(HDA_SD0FMT, 2, fmt_44k1_stereo_16 as u64);

    // Start stream.
    hda.mmio_write(HDA_SD0CTL, 4, (SD_CTL_SRST | SD_CTL_RUN) as u64);

    // Drain the BDL into the HDA audio ring.
    let mut polls = 0u32;
    while {
        let queued = hda.audio_ring().len();
        queued < pcm_len
    } {
        hda.poll(&mut mem);
        polls += 1;
        assert!(
            polls < 32,
            "HDA did not queue {pcm_len} bytes after {polls} polls (queued={})",
            hda.audio_ring().len()
        );
    }

    let drained = hda.audio_ring().drain_all();
    assert_eq!(
        drained, pcm_bytes,
        "HDA DMA did not enqueue the expected PCM bytes"
    );

    // Run the legacy DSP pipeline (decode + resample).
    let input = PcmSpec {
        format: PcmSampleFormat::I16,
        channels: 2,
        sample_rate: 44_100,
    };
    let mut proc = StreamProcessor::new(input, 48_000, 2, ResamplerKind::Linear).unwrap();

    let mut out = Vec::new();
    let mut tail = Vec::new();
    proc.process(&drained, &mut out).unwrap();
    proc.flush(&mut tail).unwrap();
    out.extend_from_slice(&tail);

    // This checksum is the contract: changes here should be intentional and justified.
    assert_eq!(hash_f32(&out), 0xadf6_a4ce_b91e_6b2f);
}
