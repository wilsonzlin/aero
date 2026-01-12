use aero_machine::{Machine, MachineConfig};
use pretty_assertions::assert_eq;

#[test]
fn vga_crtc_mono_ports_alias_colour_ports() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: false,
        enable_vga: true,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        enable_virtio_net: false,
        ..Default::default()
    };

    let mut m = Machine::new(cfg).unwrap();

    // Program cursor location high/low bytes via the "mono" CRTC ports.
    m.io_write(0x3B4, 1, 0x0E);
    m.io_write(0x3B5, 1, 0x12);
    m.io_write(0x3B4, 1, 0x0F);
    m.io_write(0x3B5, 1, 0x34);

    // Read back via the "color" CRTC ports.
    m.io_write(0x3D4, 1, 0x0E);
    let hi = m.io_read(0x3D5, 1) as u8;
    m.io_write(0x3D4, 1, 0x0F);
    let lo = m.io_read(0x3D5, 1) as u8;
    assert_eq!((hi, lo), (0x12, 0x34));

    // And the other direction: write via color, read via mono.
    m.io_write(0x3D4, 1, 0x0A);
    m.io_write(0x3D5, 1, 0x20);
    m.io_write(0x3B4, 1, 0x0A);
    let start = m.io_read(0x3B5, 1) as u8;
    assert_eq!(start, 0x20);
}

