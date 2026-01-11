use aero_audio::hda::HdaController;
use aero_audio::mem::{GuestMemory, MemoryAccess};

const REG_GCTL: u64 = 0x08;
const REG_INTCTL: u64 = 0x20;
const REG_INTSTS: u64 = 0x24;
const REG_SD0CTL: u64 = 0x80;
const REG_SD0STS: u64 = 0x83;

const SD_STS_BCIS: u8 = 1 << 2;

#[test]
fn clearing_srst_resets_stream_engine_state() {
    let mut hda = HdaController::new();
    let mut mem = GuestMemory::new(0x20_000);

    hda.mmio_write(REG_GCTL, 4, 0x1); // GCTL.CRST

    // Configure the codec converter to listen on stream 1, channel 0.
    hda.codec_mut().execute_verb(2, (0x706u32 << 8) | 0x10);

    // 48kHz, 16-bit, 2ch.
    let fmt_raw: u16 = (1 << 4) | 0x1;
    hda.codec_mut().execute_verb(2, (0x200u32 << 8) | (fmt_raw as u8 as u32));

    let bdl_base = 0x1000u64;
    let pcm_base = 0x2000u64;
    let frames = 4usize;
    let bytes_per_frame = 4usize;
    let pcm_len_bytes = frames * bytes_per_frame;

    // Fill PCM with a simple non-zero pattern.
    for n in 0..frames {
        let off = pcm_base + (n * bytes_per_frame) as u64;
        mem.write_u16(off, (0x1000 + n as u16) as u16);
        mem.write_u16(off + 2, (0x2000 + n as u16) as u16);
    }

    // One IOC BDL entry.
    mem.write_u64(bdl_base + 0, pcm_base);
    mem.write_u32(bdl_base + 8, pcm_len_bytes as u32);
    mem.write_u32(bdl_base + 12, 1);

    {
        let sd = hda.stream_mut(0);
        sd.bdpl = bdl_base as u32;
        sd.bdpu = 0;
        // Keep CBL larger than the entry so we don't wrap LPIB to 0.
        sd.cbl = 0x4000;
        sd.lvi = 0;
        sd.fmt = fmt_raw;
        // SRST | RUN | IOCE | stream number 1.
        sd.ctl = (1 << 0) | (1 << 1) | (1 << 2) | (1 << 20);
    }

    // Enable stream 0 interrupt + global interrupt.
    hda.mmio_write(REG_INTCTL, 4, (1u64 << 31) | 1u64);

    // Process enough frames to complete the BDL entry and raise IOC.
    hda.process(&mut mem, frames);

    assert_eq!(hda.stream_mut(0).lpib, pcm_len_bytes as u32);
    assert_ne!(hda.mmio_read(REG_SD0STS, 1) as u8 & SD_STS_BCIS, 0);
    assert_ne!(hda.mmio_read(REG_INTSTS, 4) as u32 & 1, 0);

    // Clear SRST while keeping RUN set (write low byte: RUN=1, SRST=0).
    hda.mmio_write(REG_SD0CTL, 1, 0x02);

    assert_eq!(hda.stream_mut(0).lpib, 0);
    assert_eq!(hda.mmio_read(REG_SD0STS, 1) as u8 & SD_STS_BCIS, 0);
    assert_eq!(hda.mmio_read(REG_INTSTS, 4) as u32 & 1, 0);
    assert!(!hda.take_irq());
}

