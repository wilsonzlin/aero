use aero_machine::{Machine, MachineConfig};
use pretty_assertions::assert_eq;

fn new_deterministic_aerogpu_machine(cfg: MachineConfig) -> Machine {
    Machine::new(MachineConfig {
        // Ensure deterministic baseline for snapshot tests.
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        enable_virtio_net: false,
        ..cfg
    })
    .unwrap()
}

fn base_cfg() -> MachineConfig {
    MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_aerogpu: true,
        enable_vga: false,
        ..Default::default()
    }
}

#[test]
fn aerogpu_snapshot_preserves_vga_dac_partial_triplet_latch() {
    let cfg = base_cfg();
    let mut m = new_deterministic_aerogpu_machine(cfg.clone());

    // Begin programming palette entry 0x10, but intentionally stop after one component so the DAC
    // is left mid-triplet.
    m.io_write(0x3C8, 1, 0x10); // DAC write index
    m.io_write(0x3C9, 1, 63); // R

    let snap = m.take_snapshot_full().unwrap();

    let mut m2 = new_deterministic_aerogpu_machine(cfg);
    m2.reset();
    m2.restore_snapshot_bytes(&snap).unwrap();

    // Continue the triplet after restore.
    m2.io_write(0x3C9, 1, 0); // G
    m2.io_write(0x3C9, 1, 0); // B

    // Validate the intended entry (0x10) was updated, not the default index (0).
    m2.io_write(0x3C7, 1, 0x10); // DAC read index
    assert_eq!(m2.io_read(0x3C9, 1) as u8, 63);
    assert_eq!(m2.io_read(0x3C9, 1) as u8, 0);
    assert_eq!(m2.io_read(0x3C9, 1) as u8, 0);

    // Completing an RGB triplet should auto-increment the write index.
    assert_eq!(m2.io_read(0x3C8, 1) as u8, 0x11);
}

#[test]
fn aerogpu_snapshot_preserves_attribute_controller_flip_flop() {
    let cfg = base_cfg();
    let mut m = new_deterministic_aerogpu_machine(cfg.clone());

    // Ensure the next 0x3C0 write is treated as an index, then write an index without providing
    // the corresponding data so the flip-flop stays in the "data" state.
    let _ = m.io_read(0x3DA, 1);
    m.io_write(0x3C0, 1, 0x21); // index 1 with bit 5 set (keep display enabled)

    let snap = m.take_snapshot_full().unwrap();

    let mut m2 = new_deterministic_aerogpu_machine(cfg);
    m2.reset();
    m2.restore_snapshot_bytes(&snap).unwrap();

    // After restore, the flip-flop should still be in the "data" state, so this write should be
    // treated as data for attribute register 1.
    m2.io_write(0x3C0, 1, 0x77);

    // Read back attribute register 1 via 0x3C1 and verify it updated.
    let _ = m2.io_read(0x3DA, 1);
    m2.io_write(0x3C0, 1, 0x21);
    assert_eq!(m2.io_read(0x3C1, 1) as u8, 0x77);
}

