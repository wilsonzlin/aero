use emulator::devices::vga::VgaDevice;
use emulator::io::PortIO;

#[test]
fn status1_vblank_toggles_and_repeats() {
    let mut vga = VgaDevice::new();

    let status = vga.port_read(0x3DA, 1) as u8;
    assert_eq!(status & 0b1000, 0, "expected vblank=0 at time 0");

    vga.tick(vga.timing().frame_ns() - vga.timing().vblank_ns());
    let status = vga.port_read(0x3DA, 1) as u8;
    assert_ne!(status & 0b1000, 0, "expected vblank=1 inside vblank window");

    vga.tick(vga.timing().vblank_ns());
    let status = vga.port_read(0x3DA, 1) as u8;
    assert_eq!(
        status & 0b1000,
        0,
        "expected vblank=0 at start of next frame"
    );
}

#[test]
fn status_read_resets_attribute_flip_flop() {
    let mut vga = VgaDevice::new();

    assert!(
        vga.attribute_flip_flop_is_index(),
        "power-on should start in index phase"
    );

    vga.port_write(0x3C0, 1, 0x00);
    assert!(
        !vga.attribute_flip_flop_is_index(),
        "writing 0x3C0 index should enter data phase"
    );

    let _ = vga.port_read(0x3DA, 1);
    assert!(
        vga.attribute_flip_flop_is_index(),
        "reading 0x3DA should reset flip-flop to index phase"
    );
}
