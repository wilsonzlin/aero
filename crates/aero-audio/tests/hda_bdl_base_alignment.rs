use aero_audio::hda::HdaController;
use aero_audio::mem::{GuestMemory, MemoryAccess};

#[test]
fn bdl_base_low_bits_are_ignored() {
    let mut hda = HdaController::new();
    let mut mem = GuestMemory::new(0x20_000);

    // Bring controller out of reset.
    hda.mmio_write(0x08, 4, 0x1); // GCTL.CRST

    // Configure the codec converter to listen on stream 1, channel 0.
    hda.codec_mut().execute_verb(2, (0x706u32 << 8) | 0x10);

    // 48kHz, 16-bit, 2ch.
    let fmt_raw: u16 = (1 << 4) | 0x1;
    hda.codec_mut()
        .execute_verb(2, (0x200u32 << 8) | (fmt_raw as u8 as u32));

    let bdl_base_aligned = 0x1000u64;
    let bdl_base_unaligned = bdl_base_aligned | 0x7f;
    let pcm_base = 0x2000u64;
    let frames = 32usize;
    let bytes_per_frame = 4usize;
    let pcm_len_bytes = frames * bytes_per_frame;

    // Fill PCM with a constant non-zero sample.
    for n in 0..frames {
        let v = 0x2000u16;
        let off = pcm_base + (n * bytes_per_frame) as u64;
        mem.write_u16(off, v);
        mem.write_u16(off + 2, v);
    }

    // One BDL entry pointing at the PCM buffer.
    mem.write_u64(bdl_base_aligned + 0, pcm_base);
    mem.write_u32(bdl_base_aligned + 8, pcm_len_bytes as u32);
    mem.write_u32(bdl_base_aligned + 12, 0);

    // Configure stream descriptor 0 with an intentionally unaligned BDPL value.
    {
        let sd = hda.stream_mut(0);
        sd.bdpl = bdl_base_unaligned as u32;
        sd.bdpu = 0;
        sd.cbl = 0x4000;
        sd.lvi = 0;
        sd.fmt = fmt_raw;
        // SRST | RUN | stream number 1.
        sd.ctl = (1 << 0) | (1 << 1) | (1 << 20);
    }

    hda.process(&mut mem, frames);

    // LPIB should advance even though BDPL had low bits set (hardware ignores them).
    assert_eq!(hda.stream_mut(0).lpib, (frames * bytes_per_frame) as u32);

    let out = hda.audio_out.pop_interleaved_stereo(8);
    assert_eq!(out.len(), 16);
    assert!(out[0].abs() > 0.001);
}
