use aero_audio::hda::HdaController;

const REG_GCTL: u64 = 0x08;
const REG_STATESTS: u64 = 0x0e;

#[test]
fn statests_reports_codec_presence_after_leaving_reset() {
    let mut hda = HdaController::new();

    // In reset, the minimal model reports no codec presence.
    assert_eq!(hda.mmio_read(REG_STATESTS, 2) as u16, 0);

    // Leaving reset should latch codec 0 presence.
    hda.mmio_write(REG_GCTL, 4, 0x1);
    assert_eq!(hda.mmio_read(REG_STATESTS, 2) as u16 & 0x1, 0x1);

    // STATESTS is RW1C.
    hda.mmio_write(REG_STATESTS, 2, 0x1);
    assert_eq!(hda.mmio_read(REG_STATESTS, 2) as u16 & 0x1, 0);
}
