use aero_audio::capture::VecDequeCaptureSource;
use aero_audio::hda::HdaController;
use aero_audio::mem::{GuestMemory, MemoryAccess};

#[test]
fn hda_capture_dma_writes_guest_memory() {
    let mut hda = HdaController::new();
    let mut mem = GuestMemory::new(0x20_000);

    // Bring controller out of reset.
    hda.mmio_write(0x08, 4, 0x1); // GCTL.CRST

    // Configure the codec ADC (NID 4) to capture from stream 2, channel 0.
    let set_stream_ch = (0x706u32 << 8) | 0x20;
    hda.codec_mut().execute_verb(4, set_stream_ch);

    // 48kHz, 16-bit, mono.
    let fmt_raw: u16 = (1 << 4) | 0x0;
    let set_fmt = (0x200u32 << 8) | (fmt_raw as u8 as u32);
    hda.codec_mut().execute_verb(4, set_fmt);

    let bdl_base = 0x1000u64;
    let pcm_base = 0x2000u64;
    let frames = 8usize;
    let bytes_per_frame = 2usize;
    let pcm_len_bytes = frames * bytes_per_frame;

    // One BDL entry pointing at the capture buffer.
    mem.write_u64(bdl_base + 0, pcm_base);
    mem.write_u32(bdl_base + 8, pcm_len_bytes as u32);
    mem.write_u32(bdl_base + 12, 0);

    // Configure stream descriptor 1 (capture).
    {
        let sd = hda.stream_mut(1);
        sd.bdpl = bdl_base as u32;
        sd.bdpu = 0;
        sd.cbl = pcm_len_bytes as u32;
        sd.lvi = 0;
        sd.fmt = fmt_raw;
        // RUN | stream number 2.
        sd.ctl = (1 << 0) | (1 << 1) | (2 << 20);
    }

    // Feed a deterministic mono waveform.
    let samples = [0.0, 0.25, 0.5, 0.75, -0.25, -0.5, -0.75, -1.0];
    let mut capture = VecDequeCaptureSource::new();
    capture.push_samples(&samples);

    hda.process_with_capture(&mut mem, frames, &mut capture);

    // We wrote exactly CBL bytes, so LPIB wraps to 0.
    assert_eq!(hda.stream_mut(1).lpib, 0);

    let mut out = vec![0u8; pcm_len_bytes];
    mem.read_physical(pcm_base, &mut out);

    let expected_samples: [i16; 8] = [0, 8192, 16384, 24576, -8192, -16384, -24576, -32768];
    let mut expected = Vec::new();
    for s in expected_samples {
        expected.extend_from_slice(&s.to_le_bytes());
    }
    assert_eq!(out, expected);
}

#[test]
fn hda_capture_resamples_from_configured_capture_rate() {
    let mut hda = HdaController::new_with_output_rate(48_000);
    // Simulate a browser mic capture graph running at 44.1kHz while the output
    // time base is 48kHz.
    hda.set_capture_sample_rate_hz(44_100);
    let mut mem = GuestMemory::new(0x20_000);

    // Bring controller out of reset.
    hda.mmio_write(0x08, 4, 0x1); // GCTL.CRST

    // Configure the codec ADC (NID 4) to capture from stream 2, channel 0.
    let set_stream_ch = (0x706u32 << 8) | 0x20;
    hda.codec_mut().execute_verb(4, set_stream_ch);

    // 48kHz, 16-bit, mono.
    let fmt_raw: u16 = (1 << 4) | 0x0;
    let set_fmt = (0x200u32 << 8) | (fmt_raw as u8 as u32);
    hda.codec_mut().execute_verb(4, set_fmt);

    let bdl_base = 0x1000u64;
    let pcm_base = 0x2000u64;
    let frames = 48usize;
    let bytes_per_frame = 2usize;
    let pcm_len_bytes = frames * bytes_per_frame;

    // One BDL entry pointing at the capture buffer.
    mem.write_u64(bdl_base + 0, pcm_base);
    mem.write_u32(bdl_base + 8, pcm_len_bytes as u32);
    mem.write_u32(bdl_base + 12, 0);

    // Configure stream descriptor 1 (capture).
    {
        let sd = hda.stream_mut(1);
        sd.bdpl = bdl_base as u32;
        sd.bdpu = 0;
        sd.cbl = pcm_len_bytes as u32;
        sd.lvi = 0;
        sd.fmt = fmt_raw;
        // RUN | stream number 2.
        sd.ctl = (1 << 0) | (1 << 1) | (2 << 20);
    }

    // Provide just enough 44.1kHz samples to synthesize 48 guest frames at 48kHz.
    let mut capture = VecDequeCaptureSource::new();
    capture.push_samples(&vec![1.0; 45]);

    hda.process_with_capture(&mut mem, frames, &mut capture);

    // We wrote exactly CBL bytes, so LPIB wraps to 0.
    assert_eq!(hda.stream_mut(1).lpib, 0);

    let mut out = vec![0u8; pcm_len_bytes];
    mem.read_physical(pcm_base, &mut out);

    let mut expected = Vec::new();
    for _ in 0..frames {
        expected.extend_from_slice(&i16::MAX.to_le_bytes());
    }
    assert_eq!(out, expected);
}
