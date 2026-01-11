use aero_audio::hda::HdaController;

const REG_GCTL: u64 = 0x08;

fn verb_12(verb_id: u16, payload8: u8) -> u32 {
    ((verb_id as u32) << 8) | payload8 as u32
}

#[test]
fn pin_power_state_round_trips() {
    let mut hda = HdaController::new();
    hda.mmio_write(REG_GCTL, 4, 0x1); // GCTL.CRST

    // Output pin (nid=3): set to D3 and read back.
    assert_eq!(hda.codec_mut().execute_verb(3, verb_12(0x705, 0x03)), 0);
    assert_eq!(hda.codec_mut().execute_verb(3, verb_12(0xF05, 0)), 0x03);

    // Mic pin (nid=5): set to D2 and read back.
    assert_eq!(hda.codec_mut().execute_verb(5, verb_12(0x705, 0x02)), 0);
    assert_eq!(hda.codec_mut().execute_verb(5, verb_12(0xF05, 0)), 0x02);

}
