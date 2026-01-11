use aero_audio::hda::{HdaController, HDA_MMIO_SIZE};

const REG_GCAP: u64 = 0x00;
const REG_VMIN: u64 = 0x02;
const REG_VMAJ: u64 = 0x03;

const REG_SD_BASE: u64 = 0x80;

const SD_REG_STS: u64 = 0x03;
const SD_REG_LVI: u64 = 0x0c;
const SD_REG_FIFOW: u64 = 0x0e;

#[test]
fn gcap_vmin_vmaj_can_be_read_as_one_dword() {
    let mut hda = HdaController::new();

    let gcap = hda.mmio_read(REG_GCAP, 2) as u32;
    let vmin = hda.mmio_read(REG_VMIN, 1) as u32;
    let vmaj = hda.mmio_read(REG_VMAJ, 1) as u32;

    // Sanity check to ensure we're not trivially packing zeroes.
    assert_eq!(vmin, 0);
    assert_eq!(vmaj, 1);

    let packed = hda.mmio_read(REG_GCAP, 4) as u32;
    assert_eq!(packed, gcap | (vmin << 16) | (vmaj << 24));
}

#[test]
fn stream_status_is_byte_accessible_and_w1c() {
    let mut hda = HdaController::new();

    // Force a status byte with BCIS (bit 2) set.
    {
        let sd = hda.stream_mut(0);
        sd.ctl = (sd.ctl & 0x00ff_ffff) | (0b111u32 << 24);
    }

    let sts_off = REG_SD_BASE + SD_REG_STS;
    assert_eq!(hda.mmio_read(sts_off, 1) as u8, 0b111);

    // W1C the BCIS bit.
    hda.mmio_write(sts_off, 1, 1 << 2);
    assert_eq!(hda.mmio_read(sts_off, 1) as u8, 0b011);
}

#[test]
fn stream_lvi_and_fifow_allow_word_access() {
    let mut hda = HdaController::new();

    let lvi_off = REG_SD_BASE + SD_REG_LVI;
    let fifow_off = REG_SD_BASE + SD_REG_FIFOW;

    hda.mmio_write(lvi_off, 2, 0x1234);
    hda.mmio_write(fifow_off, 2, 0x5678);

    assert_eq!(hda.mmio_read(lvi_off, 2) as u16, 0x1234);
    assert_eq!(hda.mmio_read(fifow_off, 2) as u16, 0x5678);

    // A 32-bit access at 0x0C spans LVI + FIFOW.
    assert_eq!(hda.mmio_read(lvi_off, 4) as u32, 0x5678_1234);
}

#[test]
fn mmio_reads_and_writes_do_not_panic_for_common_sizes() {
    let mut hda = HdaController::new();
    let limit = HDA_MMIO_SIZE as u64;

    for offset in 0..limit {
        for size in [1usize, 2, 4] {
            if offset + size as u64 > limit {
                continue;
            }
            let _ = hda.mmio_read(offset, size);
            hda.mmio_write(offset, size, 0);
        }
    }
}
