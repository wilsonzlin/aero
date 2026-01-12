use aero_machine::{Machine, MachineConfig};
use aero_snapshot as snapshot;
use pretty_assertions::assert_eq;

#[test]
fn hole_aware_ram_maps_high_memory_above_4gib_and_hole_is_open_bus() {
    let cfg = MachineConfig {
        ram_size_bytes: firmware::bios::PCIE_ECAM_BASE + 0x2000,
        // Keep the machine minimal for this regression test.
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    };

    let mut m = Machine::new(cfg).unwrap();

    // The ECAM base lies in the reserved PCI/ECAM hole; reads must behave like open bus.
    assert_eq!(m.read_physical_u8(firmware::bios::PCIE_ECAM_BASE), 0xFF);

    // High memory is remapped above 4GiB: guest physical 0x1_0000_0000 corresponds to RAM offset
    // `PCIE_ECAM_BASE` in the contiguous backing store.
    let pattern = [0xDE, 0xAD, 0xBE, 0xEF];
    m.write_physical(0x1_0000_0000, &pattern);
    assert_eq!(m.read_physical_bytes(0x1_0000_0000, pattern.len()), pattern);

    // Verify the snapshot RAM accessor sees the bytes at the *RAM offset* `PCIE_ECAM_BASE`.
    let mut buf = [0u8; 4];
    snapshot::SnapshotSource::read_ram(&m, firmware::bios::PCIE_ECAM_BASE, &mut buf).unwrap();
    assert_eq!(buf, pattern);
}

#[test]
fn snapshot_roundtrip_preserves_high_memory_contents() {
    let cfg = MachineConfig {
        ram_size_bytes: firmware::bios::PCIE_ECAM_BASE + 0x2000,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    };

    let mut src = Machine::new(cfg.clone()).unwrap();

    // Dirty snapshots are diffs and require a parent snapshot id. Take a (compressed) full base
    // snapshot first, then record just the high-memory modifications as a dirty snapshot.
    let base = src.take_snapshot_full().unwrap();

    // Write a recognizable pattern into the remapped high-memory portion (>= 4GiB).
    let pattern: Vec<u8> = (0..0x2000).map(|i| (i as u8).wrapping_mul(37)).collect();
    src.write_physical(0x1_0000_0000, &pattern);
    let diff = src.take_snapshot_dirty().unwrap();

    let mut restored = Machine::new(cfg).unwrap();
    restored.restore_snapshot_bytes(&base).unwrap();
    restored.restore_snapshot_bytes(&diff).unwrap();

    assert_eq!(
        restored.read_physical_bytes(0x1_0000_0000, pattern.len()),
        pattern
    );
}
