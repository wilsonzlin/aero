use aero_machine::{Machine, MachineConfig};
use aero_snapshot as snapshot;
use pretty_assertions::assert_eq;
use std::sync::Mutex;

// These regression tests intentionally configure a VM with >2.75GiB of guest RAM to ensure the
// PC PCI/ECAM hole behavior is correct. Serializing the tests avoids running multiple such VMs in
// parallel under the default Rust test harness, which can otherwise spike memory usage.
static TEST_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn hole_aware_ram_maps_high_memory_above_4gib_and_hole_is_open_bus() {
    let _guard = TEST_LOCK.lock().unwrap();
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
    let _guard = TEST_LOCK.lock().unwrap();
    let cfg = MachineConfig {
        ram_size_bytes: firmware::bios::PCIE_ECAM_BASE + 0x2000,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    };

    let mut src = Machine::new(cfg.clone()).unwrap();

    // Dirty snapshots are diffs and require a parent snapshot id. For this regression test we can
    // avoid taking a full base snapshot (which would otherwise scan ~2.75GiB of RAM) by manually
    // advancing the snapshot id chain with `snapshot_meta()`.
    let _ = snapshot::SnapshotSource::take_dirty_pages(&mut src);
    let base_meta = snapshot::SnapshotSource::snapshot_meta(&mut src);

    // Write a recognizable pattern into the remapped high-memory portion (>= 4GiB).
    let pattern: Vec<u8> = (0..0x2000).map(|i| (i as u8).wrapping_mul(37)).collect();
    src.write_physical(0x1_0000_0000, &pattern);
    let diff = src.take_snapshot_dirty().unwrap();

    // Drop the source machine before constructing the restore target to keep peak memory usage
    // bounded (these tests use a multi-gigabyte RAM config).
    drop(src);

    let mut restored = Machine::new(cfg).unwrap();
    snapshot::SnapshotTarget::restore_meta(&mut restored, base_meta);
    restored.restore_snapshot_bytes(&diff).unwrap();

    assert_eq!(
        restored.read_physical_bytes(0x1_0000_0000, pattern.len()),
        pattern
    );
}
