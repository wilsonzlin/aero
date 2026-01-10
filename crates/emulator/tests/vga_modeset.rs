use emulator::devices::vga::VgaDevice;
use emulator::io::PortIO;

#[test]
fn modeset_13h_programs_chain4_and_resolution() {
    let mut vga = VgaDevice::new();
    vga.set_legacy_mode(0x13, true);

    let derived = vga.derived_state();
    assert!(derived.is_graphics);
    assert_eq!((derived.width, derived.height), (320, 200));
    assert!(derived.chain4);
    assert_eq!(derived.pitch_bytes, 320);

    // Sequencer[4] should have chain-4 enabled (bit 3 in the standard tables).
    vga.port_write(0x3C4, 1, 0x04);
    let seq4 = vga.port_read(0x3C5, 1) as u8;
    assert_eq!(seq4, 0x0E);

    // Graphics controller memory map should be A0000 64KiB window.
    vga.port_write(0x3CE, 1, 0x06);
    let gc6 = vga.port_read(0x3CF, 1) as u8;
    assert_eq!(gc6, 0x05);

    // VRAM clear should have zeroed at least the visible 64KiB window (chain4 -> 16KiB per plane).
    assert!(vga.vram()[0..0x4000].iter().all(|&b| b == 0));
    assert!(vga.vram()[0x10000..0x10000 + 0x4000]
        .iter()
        .all(|&b| b == 0));
}

#[test]
fn modeset_03h_programs_text_mode_and_clears_text_buffer() {
    let mut vga = VgaDevice::new();
    vga.set_legacy_mode(0x03, true);

    let derived = vga.derived_state();
    assert!(!derived.is_graphics);
    assert_eq!(derived.text_columns, 80);
    assert_eq!(derived.text_rows, 25);
    assert_eq!(derived.vram_window_base, 0xB8000);

    // CRTC[0x13] offset should yield 160 bytes/row in text mode.
    assert_eq!(derived.pitch_bytes, 160);

    // VRAM clear should have written spaces + attribute 0x07.
    assert_eq!(vga.vram()[0], 0x20);
    assert_eq!(vga.vram()[0x10000], 0x07);
}

#[test]
fn modeset_12h_programs_planar_mode_and_pitch() {
    let mut vga = VgaDevice::new();
    vga.set_legacy_mode(0x12, true);

    let derived = vga.derived_state();
    assert!(derived.is_graphics);
    assert!(!derived.chain4);
    assert_eq!((derived.width, derived.height), (640, 480));

    // Planar 640x480: 640/8 = 80 bytes per scanline per plane.
    assert_eq!(derived.pitch_bytes, 80);

    // Graphics controller memory map should be A0000 64KiB window.
    assert_eq!(derived.vram_window_base, 0xA0000);

    // Verify a representative CRTC register via port reads.
    vga.port_write(0x3D4, 1, 0x13);
    let crtc13 = vga.port_read(0x3D5, 1) as u8;
    assert_eq!(crtc13, 0x28);
}
