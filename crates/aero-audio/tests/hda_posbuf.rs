use aero_audio::hda::HdaController;
use aero_audio::mem::{GuestMemory, MemoryAccess};

const REG_GCTL: u64 = 0x08;
const REG_DPLBASE: u64 = 0x70;
const REG_DPUBASE: u64 = 0x74;

fn configure_basic_stream(mem: &mut GuestMemory, hda: &mut HdaController, frames: usize, cbl: u32) {
    // Configure the codec converter to listen on stream 1, channel 0.
    let set_stream_ch = (0x706u32 << 8) | 0x10;
    hda.codec_mut().execute_verb(2, set_stream_ch);

    // 48kHz, 16-bit, 2ch.
    let fmt_raw: u16 = (1 << 4) | 0x1;
    let set_fmt = (0x200u32 << 8) | (fmt_raw as u8 as u32);
    hda.codec_mut().execute_verb(2, set_fmt);

    let bdl_base = 0x1000u64;
    let pcm_base = 0x2000u64;
    let bytes_per_frame = 4usize;
    let pcm_len_bytes = frames * bytes_per_frame;

    // Fill PCM with a simple ramp.
    for n in 0..frames {
        let v = n as u16;
        let off = pcm_base + (n * bytes_per_frame) as u64;
        mem.write_u16(off, v);
        mem.write_u16(off + 2, v);
    }

    mem.write_u64(bdl_base + 0, pcm_base);
    mem.write_u32(bdl_base + 8, pcm_len_bytes as u32);
    mem.write_u32(bdl_base + 12, 0);

    let sd = hda.stream_mut(0);
    sd.bdpl = bdl_base as u32;
    sd.bdpu = 0;
    sd.cbl = cbl;
    sd.lvi = 0;
    sd.fmt = fmt_raw;
    // RUN | stream number 1.
    sd.ctl = (1 << 1) | (1 << 20);
}

#[test]
fn position_buffer_not_updated_when_disabled() {
    let mut hda = HdaController::new();
    let mut mem = GuestMemory::new(0x20_000);

    hda.mmio_write(REG_GCTL, 4, 0x1); // GCTL.CRST

    let posbuf_base = 0x1800u64;
    mem.write_u32(posbuf_base, 0xdead_beef);
    hda.mmio_write(REG_DPUBASE, 4, 0);
    hda.mmio_write(REG_DPLBASE, 4, posbuf_base);

    configure_basic_stream(&mut mem, &mut hda, 480, 0x4000);
    hda.process(&mut mem, 120);

    assert_eq!(mem.read_u32(posbuf_base), 0xdead_beef);
}

#[test]
fn position_buffer_tracks_lpib_progress() {
    let mut hda = HdaController::new();
    let mut mem = GuestMemory::new(0x20_000);

    hda.mmio_write(REG_GCTL, 4, 0x1); // GCTL.CRST

    let posbuf_base = 0x1800u64;
    hda.mmio_write(REG_DPUBASE, 4, 0);
    hda.mmio_write(REG_DPLBASE, 4, posbuf_base | 0x1); // enable

    configure_basic_stream(&mut mem, &mut hda, 480, 0x4000);
    hda.process(&mut mem, 120);

    let expected_lpib = (120 * 4) as u32;
    assert_eq!(hda.stream_mut(0).lpib, expected_lpib);
    assert_eq!(mem.read_u32(posbuf_base), expected_lpib);
}

#[test]
fn position_buffer_reflects_cbl_wraparound() {
    let mut hda = HdaController::new();
    let mut mem = GuestMemory::new(0x20_000);

    hda.mmio_write(REG_GCTL, 4, 0x1); // GCTL.CRST

    let posbuf_base = 0x1800u64;
    hda.mmio_write(REG_DPUBASE, 4, 0);
    hda.mmio_write(REG_DPLBASE, 4, posbuf_base | 0x1); // enable

    // 48kHz, 16-bit stereo => 4 bytes/frame.
    let frames = 480usize; // 10ms
    let cbl_bytes = (frames * 4) as u32;
    configure_basic_stream(&mut mem, &mut hda, frames, cbl_bytes);

    // Consume exactly one buffer worth of frames, which wraps LPIB to 0.
    hda.process(&mut mem, frames);

    assert_eq!(hda.stream_mut(0).lpib, 0);
    assert_eq!(mem.read_u32(posbuf_base), 0);
}
