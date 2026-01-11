use aero_audio::capture::VecDequeCaptureSource;
use aero_audio::hda::HdaController;
use aero_audio::mem::{GuestMemory, MemoryAccess};

const REG_GCTL: u64 = 0x08;
const REG_INTCTL: u64 = 0x20;
const REG_INTSTS: u64 = 0x24;
const REG_SD_BASE: u64 = 0x80;
const SD_STRIDE: u64 = 0x20;
const SD_STS_BCIS: u8 = 1 << 2;

#[test]
fn stream_status_byte_is_rw1c_and_clears_intsts() {
    let mut hda = HdaController::new();
    let mut mem = GuestMemory::new(0x20_000);

    hda.mmio_write(REG_GCTL, 4, 0x1); // GCTL.CRST

    // Codec ADC (NID 4) -> stream tag 2.
    hda.codec_mut().execute_verb(4, (0x706u32 << 8) | 0x20);
    // 48kHz, 16-bit, mono.
    let fmt_raw: u16 = (1 << 4) | 0x0;
    hda.codec_mut().execute_verb(4, (0x200u32 << 8) | (fmt_raw as u8 as u32));

    let bdl_base = 0x1000u64;
    let pcm_base = 0x2000u64;
    let frames = 4usize;
    let bytes_per_frame = 2usize;
    let pcm_len_bytes = frames * bytes_per_frame;

    mem.write_u64(bdl_base + 0, pcm_base);
    mem.write_u32(bdl_base + 8, pcm_len_bytes as u32);
    mem.write_u32(bdl_base + 12, 1); // IOC

    // Program capture stream (SD1).
    {
        let sd = hda.stream_mut(1);
        sd.bdpl = bdl_base as u32;
        sd.bdpu = 0;
        sd.cbl = pcm_len_bytes as u32;
        sd.lvi = 0;
        sd.fmt = fmt_raw;
        // RUN | IOCE | stream tag 2.
        sd.ctl = (1 << 0) | (1 << 1) | (1 << 2) | (2 << 20);
    }

    // Enable stream1 interrupts + global interrupt.
    hda.mmio_write(REG_INTCTL, 4, (1u64 << 31) | (1u64 << 1));

    let mut capture = VecDequeCaptureSource::new();
    capture.push_samples(&[0.25; 16]);
    hda.process_with_capture(&mut mem, frames, &mut capture);

    // Interrupt + BCIS latched.
    assert_ne!(hda.mmio_read(REG_INTSTS, 4) as u32 & (1 << 1), 0);

    let sd1_sts = REG_SD_BASE + SD_STRIDE * 1 + 0x03;
    assert_ne!(hda.mmio_read(sd1_sts, 1) as u8 & SD_STS_BCIS, 0);

    let ctl_before = hda.stream_mut(1).ctl & 0x00ff_ffff;
    hda.mmio_write(sd1_sts, 1, SD_STS_BCIS as u64);
    let ctl_after = hda.stream_mut(1).ctl & 0x00ff_ffff;
    assert_eq!(ctl_before, ctl_after);

    // Clearing SDSTS.BCIS clears the SIS bit in INTSTS and the SDSTS byte.
    assert_eq!(hda.mmio_read(sd1_sts, 1) as u8 & SD_STS_BCIS, 0);
    assert_eq!(hda.mmio_read(REG_INTSTS, 4) as u32 & (1 << 1), 0);
}

#[test]
fn bcis_latches_without_ioce() {
    let mut hda = HdaController::new();
    let mut mem = GuestMemory::new(0x20_000);

    hda.mmio_write(REG_GCTL, 4, 0x1); // GCTL.CRST

    // Codec ADC (NID 4) -> stream tag 2.
    hda.codec_mut().execute_verb(4, (0x706u32 << 8) | 0x20);
    let fmt_raw: u16 = (1 << 4) | 0x0;
    hda.codec_mut().execute_verb(4, (0x200u32 << 8) | (fmt_raw as u8 as u32));

    let bdl_base = 0x1000u64;
    let pcm_base = 0x2000u64;
    let frames = 4usize;
    let bytes_per_frame = 2usize;
    let pcm_len_bytes = frames * bytes_per_frame;

    mem.write_u64(bdl_base + 0, pcm_base);
    mem.write_u32(bdl_base + 8, pcm_len_bytes as u32);
    mem.write_u32(bdl_base + 12, 1); // IOC

    {
        let sd = hda.stream_mut(1);
        sd.bdpl = bdl_base as u32;
        sd.bdpu = 0;
        sd.cbl = pcm_len_bytes as u32;
        sd.lvi = 0;
        sd.fmt = fmt_raw;
        // RUN | stream tag 2 (IOCE disabled).
        sd.ctl = (1 << 0) | (1 << 1) | (2 << 20);
    }

    // Enable stream1 interrupts + global interrupt (should not matter; IOCE is off).
    hda.mmio_write(REG_INTCTL, 4, (1u64 << 31) | (1u64 << 1));

    let mut capture = VecDequeCaptureSource::new();
    capture.push_samples(&[0.25; 16]);
    hda.process_with_capture(&mut mem, frames, &mut capture);

    let sd1_sts = REG_SD_BASE + SD_STRIDE * 1 + 0x03;
    assert_ne!(hda.mmio_read(sd1_sts, 1) as u8 & SD_STS_BCIS, 0);
    assert_eq!(hda.mmio_read(REG_INTSTS, 4) as u32 & (1 << 1), 0);
}
