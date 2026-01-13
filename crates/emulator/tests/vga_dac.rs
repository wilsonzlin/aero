use emulator::devices::vga::VgaDac;

#[test]
fn dac_write_auto_increment_and_readback() {
    let mut dac = VgaDac::new();

    // Write two palette entries starting at index 0.
    dac.port_write(0x3C8, 0x00);
    // Entry 0: treat as 8-bit because one component is > 63. This ensures we handle common
    // 8-bit palette programming even when some components are in the "dark" (<= 63) range.
    dac.port_write(0x3C9, 0x20); // 0x20 >> 2 = 0x08
    dac.port_write(0x3C9, 0x10); // 0x10 >> 2 = 0x04
    dac.port_write(0x3C9, 0xFF); // 0xFF >> 2 = 0x3F
    dac.port_write(0x3C9, 0x04);
    dac.port_write(0x3C9, 0x05);
    dac.port_write(0x3C9, 0x06);

    assert_eq!(dac.palette_rgb6()[0], [0x08, 0x04, 0x3F]);
    assert_eq!(dac.palette_rgb6()[1], [0x04, 0x05, 0x06]);

    // Read them back using the DAC read index.
    dac.port_write(0x3C7, 0x00);
    assert_eq!(dac.port_read(0x3C9), 0x08);
    assert_eq!(dac.port_read(0x3C9), 0x04);
    assert_eq!(dac.port_read(0x3C9), 0x3F);
    assert_eq!(dac.port_read(0x3C9), 0x04);
    assert_eq!(dac.port_read(0x3C9), 0x05);
    assert_eq!(dac.port_read(0x3C9), 0x06);
}

#[test]
fn pel_mask_port_read_write_round_trips() {
    let mut dac = VgaDac::new();
    dac.port_write(0x3C6, 0x0F);
    assert_eq!(dac.port_read(0x3C6), 0x0F);
    assert_eq!(dac.pel_mask(), 0x0F);
}
