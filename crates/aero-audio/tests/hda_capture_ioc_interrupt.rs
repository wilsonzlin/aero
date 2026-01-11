use aero_audio::capture::VecDequeCaptureSource;
use aero_audio::hda::HdaController;
use aero_audio::mem::{GuestMemory, MemoryAccess};

const REG_GCTL: u64 = 0x08;
const REG_INTCTL: u64 = 0x20;

#[test]
fn hda_capture_ioc_interrupt() {
    let mut hda = HdaController::new();
    let mut mem = GuestMemory::new(0x20_000);

    hda.mmio_write(REG_GCTL, 4, 0x1); // GCTL.CRST

    // Codec ADC (NID 4) -> stream 2.
    let set_stream_ch = (0x706u32 << 8) | 0x20;
    hda.codec_mut().execute_verb(4, set_stream_ch);

    let fmt_raw: u16 = (1 << 4) | 0x0; // 48kHz, 16-bit, mono
    let set_fmt = (0x200u32 << 8) | (fmt_raw as u8 as u32);
    hda.codec_mut().execute_verb(4, set_fmt);

    let bdl_base = 0x1000u64;
    let pcm_base = 0x2000u64;
    let frames = 4usize;
    let bytes_per_frame = 2usize;
    let pcm_len_bytes = frames * bytes_per_frame;

    mem.write_u64(bdl_base + 0, pcm_base);
    mem.write_u32(bdl_base + 8, pcm_len_bytes as u32);
    mem.write_u32(bdl_base + 12, 1); // IOC

    let sd = hda.stream_mut(1);
    sd.bdpl = bdl_base as u32;
    sd.bdpu = 0;
    sd.cbl = pcm_len_bytes as u32;
    sd.lvi = 0;
    sd.fmt = fmt_raw;
    // RUN | IOCE | stream number 2.
    sd.ctl = (1 << 0) | (1 << 1) | (1 << 2) | (2 << 20);

    // Enable stream 1 interrupts + global interrupt.
    hda.mmio_write(REG_INTCTL, 4, (1u64 << 31) | (1u64 << 1));

    let mut capture = VecDequeCaptureSource::new();
    capture.push_samples(&[0.5; 8]);

    hda.process_with_capture(&mut mem, frames, &mut capture);
    assert!(hda.take_irq());
}
