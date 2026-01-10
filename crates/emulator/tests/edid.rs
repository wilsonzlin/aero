use emulator::devices::vga::vbe;

#[test]
fn edid_has_valid_header_and_checksum() {
    let edid = vbe::read_edid(0).expect("missing base EDID");
    assert_eq!(
        &edid[0..8],
        &[0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00]
    );

    let sum = edid.iter().fold(0u8, |acc, &b| acc.wrapping_add(b));
    assert_eq!(sum, 0);
}

#[test]
fn edid_includes_1024x768_dtd() {
    let edid = vbe::read_edid(0).expect("missing base EDID");
    assert_eq!(
        &edid[54..72],
        &[
            0x64, 0x19, 0x00, 0x40, 0x41, 0x00, 0x26, 0x30, 0x18, 0x88, 0x36, 0x00, 0x54, 0x0E,
            0x11, 0x00, 0x00, 0x18
        ]
    );
}
