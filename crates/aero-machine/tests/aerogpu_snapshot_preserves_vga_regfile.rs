use aero_machine::{Machine, MachineConfig};
use pretty_assertions::assert_eq;

fn new_deterministic_aerogpu_machine() -> Machine {
    Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_aerogpu: true,
        enable_vga: false,
        // Keep test output deterministic.
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        enable_virtio_net: false,
        ..Default::default()
    })
    .unwrap()
}

#[test]
fn aerogpu_snapshot_preserves_legacy_vga_register_file() {
    let mut m = new_deterministic_aerogpu_machine();

    // ---------------------------------------------------------------------
    // Program a few representative VGA port registers.
    // ---------------------------------------------------------------------
    m.io_write(0x3C2, 1, 0x67); // Misc Output

    // Sequencer: index 0x02 = 0xBE.
    m.io_write(0x3C4, 1, 0x02);
    m.io_write(0x3C5, 1, 0xBE);

    // Graphics Controller: index 0x06 = 0x4F.
    m.io_write(0x3CE, 1, 0x06);
    m.io_write(0x3CF, 1, 0x4F);

    // CRTC (color base): pick a register that is *not* touched by the machine's cursor/BDA sync
    // that runs in `post_restore` (that sync writes cursor regs 0x0A/0x0B/0x0E/0x0F).
    //
    // Use 0x12 (Vertical Display End) as an arbitrary-but-stable byte for snapshot coverage.
    m.io_write(0x3D4, 1, 0x12);
    m.io_write(0x3D5, 1, 0x34);

    // Leave the index registers at non-zero values to ensure they are snapshotted too.
    m.io_write(0x3C4, 1, 0x07);
    m.io_write(0x3CE, 1, 0x09);

    let snap = m.take_snapshot_full().unwrap();

    let mut m2 = new_deterministic_aerogpu_machine();
    m2.reset();
    m2.restore_snapshot_bytes(&snap).unwrap();

    // Misc Output should be readable back via both the write port (0x3C2) and the canonical read
    // port (0x3CC).
    assert_eq!(m2.io_read(0x3C2, 1) as u8, 0x67);
    assert_eq!(m2.io_read(0x3CC, 1) as u8, 0x67);

    // Index registers should restore.
    assert_eq!(m2.io_read(0x3C4, 1) as u8, 0x07);
    assert_eq!(m2.io_read(0x3CE, 1) as u8, 0x09);

    // Data registers should restore.
    m2.io_write(0x3C4, 1, 0x02);
    assert_eq!(m2.io_read(0x3C5, 1) as u8, 0xBE);

    m2.io_write(0x3CE, 1, 0x06);
    assert_eq!(m2.io_read(0x3CF, 1) as u8, 0x4F);

    m2.io_write(0x3D4, 1, 0x12);
    assert_eq!(m2.io_read(0x3D5, 1) as u8, 0x34);
}
