use emulator::devices::vga::VgaDevice;
use emulator::io::PortIO;

#[test]
fn video_subsystem_enable_latches_and_reads_back() {
    let mut vga = VgaDevice::new();

    vga.port_write(0x3C3, 1, 0x00);
    assert_eq!(vga.port_read(0x3C3, 1) as u8, 0x00);

    vga.port_write(0x3C3, 1, 0x01);
    assert_eq!(vga.port_read(0x3C3, 1) as u8, 0x01);
}

#[test]
fn feature_control_round_trip() {
    let mut vga = VgaDevice::new();

    vga.port_write(0x3DA, 1, 0x55);
    assert_eq!(vga.port_read(0x3CA, 1) as u8, 0x55);

    // In colour I/O mode, the mono alias should be ignored.
    vga.port_write(0x3BA, 1, 0xAA);
    assert_eq!(vga.port_read(0x3CA, 1) as u8, 0x55);

    // Switch to mono I/O decode: feature control write port becomes 0x3BA.
    vga.port_write(0x3C2, 1, 0x00);
    vga.port_write(0x3BA, 1, 0xAA);
    assert_eq!(vga.port_read(0x3CA, 1) as u8, 0xAA);
}

#[test]
fn input_status_1_read_resets_attribute_controller_flip_flop() {
    let mut vga = VgaDevice::new();

    // Put AC into "data phase" by writing an index.
    vga.port_write(0x3C0, 1, 0x10);

    // Exercise Feature Control writes (same port number as Input Status 1).
    vga.port_write(0x3DA, 1, 0x55);
    assert_eq!(vga.port_read(0x3CA, 1) as u8, 0x55);

    // Reading Input Status 1 resets the flip-flop back to index phase.
    vga.port_read(0x3DA, 1);

    // Now this write should be treated as an index, not data for 0x10.
    vga.port_write(0x3C0, 1, 0x0F);
    vga.port_write(0x3C0, 1, 0x22);

    assert_eq!(vga.port_read(0x3C1, 1) as u8, 0x22);
}
