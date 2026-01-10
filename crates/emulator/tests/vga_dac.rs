use emulator::devices::vga::VgaDac;

#[test]
fn dac_write_auto_increment_and_readback() {
    let mut dac = VgaDac::new();

    // Write two palette entries starting at index 0.
    dac.port_write(0x3C8, 0x00);
    dac.port_write(0x3C9, 0x01);
    dac.port_write(0x3C9, 0x02);
    dac.port_write(0x3C9, 0x03);
    dac.port_write(0x3C9, 0x04);
    dac.port_write(0x3C9, 0x05);
    dac.port_write(0x3C9, 0x06);

    assert_eq!(dac.palette_rgb6()[0], [0x01, 0x02, 0x03]);
    assert_eq!(dac.palette_rgb6()[1], [0x04, 0x05, 0x06]);

    // Read them back using the DAC read index.
    dac.port_write(0x3C7, 0x00);
    assert_eq!(dac.port_read(0x3C9), 0x01);
    assert_eq!(dac.port_read(0x3C9), 0x02);
    assert_eq!(dac.port_read(0x3C9), 0x03);
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
