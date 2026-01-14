use aero_machine::{Machine, MachineConfig};
use pretty_assertions::assert_eq;

fn base_cfg() -> MachineConfig {
    MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_aerogpu: true,
        enable_vga: false,
        // Keep the machine minimal/deterministic for this snapshot test.
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        enable_virtio_net: false,
        ..Default::default()
    }
}

#[test]
fn aerogpu_snapshot_preserves_vga_port_register_file() {
    let cfg = base_cfg();
    let mut m = Machine::new(cfg.clone()).unwrap();

    // Program a selection of VGA port registers through the AeroGPU legacy VGA frontend.
    m.io_write(0x3C2, 1, 0x67); // misc output

    m.io_write(0x3C4, 1, 0x02); // seq index
    m.io_write(0x3C5, 1, 0xBE); // seq data

    m.io_write(0x3CE, 1, 0x06); // gc index
    m.io_write(0x3CF, 1, 0x4F); // gc data

    // Avoid cursor-related CRTC regs (0x0A/0x0B/0x0E/0x0F) because `Machine::post_restore` may
    // rewrite them when resyncing the BIOS cursor state.
    m.io_write(0x3D4, 1, 0x19); // crtc index
    m.io_write(0x3D5, 1, 0x12); // crtc data

    // Sanity check readback before snapshot.
    assert_eq!(m.io_read(0x3C2, 1) as u8, 0x67);
    assert_eq!(m.io_read(0x3CC, 1) as u8, 0x67);

    assert_eq!(m.io_read(0x3C4, 1) as u8, 0x02);
    m.io_write(0x3C4, 1, 0x02);
    assert_eq!(m.io_read(0x3C5, 1) as u8, 0xBE);

    assert_eq!(m.io_read(0x3CE, 1) as u8, 0x06);
    m.io_write(0x3CE, 1, 0x06);
    assert_eq!(m.io_read(0x3CF, 1) as u8, 0x4F);

    m.io_write(0x3D4, 1, 0x19);
    assert_eq!(m.io_read(0x3D5, 1) as u8, 0x12);

    let snap = m.take_snapshot_full().unwrap();

    let mut m2 = Machine::new(cfg).unwrap();
    m2.reset();
    m2.restore_snapshot_bytes(&snap).unwrap();

    // Validate register file survives snapshot/restore.
    assert_eq!(m2.io_read(0x3C2, 1) as u8, 0x67);
    assert_eq!(m2.io_read(0x3CC, 1) as u8, 0x67);

    assert_eq!(m2.io_read(0x3C4, 1) as u8, 0x02);
    m2.io_write(0x3C4, 1, 0x02);
    assert_eq!(m2.io_read(0x3C5, 1) as u8, 0xBE);

    assert_eq!(m2.io_read(0x3CE, 1) as u8, 0x06);
    m2.io_write(0x3CE, 1, 0x06);
    assert_eq!(m2.io_read(0x3CF, 1) as u8, 0x4F);

    m2.io_write(0x3D4, 1, 0x19);
    assert_eq!(m2.io_read(0x3D5, 1) as u8, 0x12);
}
