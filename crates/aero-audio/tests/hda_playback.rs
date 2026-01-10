use aero_audio::hda::HdaController;
use aero_audio::mem::{GuestMemory, MemoryAccess};

#[test]
fn hda_dma_tone_reaches_ring_buffer() {
    let mut hda = HdaController::new();
    let mut mem = GuestMemory::new(0x20_000);

    // Bring controller out of reset.
    hda.mmio_write(0x08, 4, 0x1); // GCTL.CRST

    // Configure the codec converter to listen on stream 1, channel 0.
    // SET_STREAM_CHANNEL: verb 0x706, payload = stream<<4 | channel
    let set_stream_ch = (0x706u32 << 8) | 0x10;
    hda.codec_mut().execute_verb(2, set_stream_ch);

    // Stream format: 48kHz, 16-bit, 2ch.
    let fmt_raw: u16 = (1 << 4) | 0x1;
    // SET_CONVERTER_FORMAT (4-bit verb group 0x2 encoded in low 16 bits)
    let set_fmt = (0x200u32 << 8) | (fmt_raw as u8 as u32);
    hda.codec_mut().execute_verb(2, set_fmt);

    // Guest buffer layout.
    let bdl_base = 0x1000u64;
    let pcm_base = 0x2000u64;
    let frames = 480usize; // 10ms at 48kHz
    let bytes_per_frame = 4usize; // 16-bit stereo
    let pcm_len_bytes = frames * bytes_per_frame;

    // Fill PCM buffer with a 440Hz sine wave.
    let freq_hz = 440.0f32;
    let sr_hz = 48_000.0f32;
    for n in 0..frames {
        let t = n as f32 / sr_hz;
        let s = (2.0 * core::f32::consts::PI * freq_hz * t).sin() * 0.5;
        let v = (s * i16::MAX as f32) as i16;
        let off = pcm_base + (n * bytes_per_frame) as u64;
        mem.write_u16(off, v as u16);
        mem.write_u16(off + 2, v as u16);
    }

    // One BDL entry pointing at the PCM buffer, IOC=1.
    mem.write_u64(bdl_base + 0, pcm_base);
    mem.write_u32(bdl_base + 8, pcm_len_bytes as u32);
    mem.write_u32(bdl_base + 12, 1);

    // Configure stream descriptor 0.
    {
        let sd = hda.stream_mut(0);
        sd.bdpl = bdl_base as u32;
        sd.bdpu = 0;
        sd.cbl = pcm_len_bytes as u32;
        sd.lvi = 0;
        sd.fmt = fmt_raw;
        // RUN | IOCE | stream number 1.
        sd.ctl = (1 << 1) | (1 << 2) | (1 << 20);
    }

    // Enable stream interrupts.
    hda.mmio_write(0x20, 4, (1u64 << 31) | 1u64); // INTCTL.GIE + stream0 enable

    // Run enough host time to consume the buffer once.
    hda.process(&mut mem, frames);

    // IOC should have fired and LPIB should wrap to 0.
    assert!(hda.take_irq());
    assert_eq!(hda.stream_mut(0).lpib, 0);

    // The ring buffer should contain the rendered tone.
    let out = hda.audio_out.pop_interleaved_stereo(128);
    assert_eq!(out.len(), 256);
    // Frame 0 is ~0.
    assert!(out[0].abs() < 0.01);
    assert!(out[1].abs() < 0.01);
    // Frame 1 should be non-zero and within amplitude.
    assert!(out[2].abs() > 0.001);
    assert!(out[2].abs() < 0.6);
}
