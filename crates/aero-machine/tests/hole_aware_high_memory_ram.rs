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
    // Writes to the hole must be ignored (open bus).
    m.write_physical_u8(firmware::bios::PCIE_ECAM_BASE, 0x00);
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

#[test]
fn dirty_pages_for_remapped_high_ram_are_reported_in_ram_offset_space() {
    let cfg = MachineConfig {
        ram_size_bytes: firmware::bios::PCIE_ECAM_BASE + 0x2000,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    };

    let mut m = Machine::new(cfg).unwrap();

    // Machine::new performs a reset which clears dirty pages.
    assert_eq!(
        snapshot::SnapshotSource::take_dirty_pages(&mut m).unwrap(),
        Vec::<u64>::new()
    );

    // Write into the remapped high-memory region (>= 4GiB).
    m.write_physical_u8(0x1_0000_0000, 0xAA);

    // Dirty pages must be indexed in contiguous RAM-offset space, so this should correspond to the
    // page containing `PCIE_ECAM_BASE`.
    let expected = firmware::bios::PCIE_ECAM_BASE / u64::from(snapshot::SnapshotSource::dirty_page_size(&m));
    assert_eq!(
        snapshot::SnapshotSource::take_dirty_pages(&mut m).unwrap(),
        vec![expected]
    );
}

#[test]
fn snapshot_read_ram_straddles_low_high_boundary() {
    let cfg = MachineConfig {
        ram_size_bytes: firmware::bios::PCIE_ECAM_BASE + 0x2000,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    };

    let mut m = Machine::new(cfg).unwrap();

    let low_ram_end = firmware::bios::PCIE_ECAM_BASE;
    let low_phys = low_ram_end - 0x10;
    let high_phys = 0x1_0000_0000;

    m.write_physical(low_phys, &[0xAA; 0x10]);
    m.write_physical(high_phys, &[0xBB; 0x10]);

    let mut buf = [0u8; 0x20];
    snapshot::SnapshotSource::read_ram(&m, low_phys, &mut buf).unwrap();
    assert_eq!(&buf[..0x10], &[0xAA; 0x10]);
    assert_eq!(&buf[0x10..], &[0xBB; 0x10]);
}

#[test]
fn snapshot_write_ram_straddles_low_high_boundary() {
    let cfg = MachineConfig {
        ram_size_bytes: firmware::bios::PCIE_ECAM_BASE + 0x2000,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    };

    let mut m = Machine::new(cfg).unwrap();

    let low_ram_end = firmware::bios::PCIE_ECAM_BASE;
    let low_offset = low_ram_end - 0x10;
    let high_phys = 0x1_0000_0000;

    let mut data = [0u8; 0x20];
    data[..0x10].fill(0x11);
    data[0x10..].fill(0x22);

    snapshot::SnapshotTarget::write_ram(&mut m, low_offset, &data).unwrap();

    assert_eq!(m.read_physical_bytes(low_offset, 0x10), vec![0x11; 0x10]);
    assert_eq!(m.read_physical_bytes(high_phys, 0x10), vec![0x22; 0x10]);
}

#[test]
fn physical_read_across_4gib_boundary_is_contiguous() {
    let cfg = MachineConfig {
        ram_size_bytes: firmware::bios::PCIE_ECAM_BASE + 0x2000,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    };

    let mut m = Machine::new(cfg).unwrap();

    let pattern: Vec<u8> = (0..0x10).collect();
    let prefix = m.read_physical_bytes(0xFFFF_FFF0, 0x10);
    m.write_physical(0x1_0000_0000, &pattern);

    let got = m.read_physical_bytes(0xFFFF_FFF0, 0x20);
    assert_eq!(&got[..0x10], &prefix);
    assert_eq!(&got[0x10..], &pattern);
}

#[test]
fn physical_write_across_4gib_boundary_ignores_rom_and_writes_high_ram() {
    let cfg = MachineConfig {
        ram_size_bytes: firmware::bios::PCIE_ECAM_BASE + 0x2000,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    };

    let mut m = Machine::new(cfg).unwrap();

    let rom_before = m.read_physical_bytes(0xFFFF_FFF0, 0x10);
    let data: Vec<u8> = (0..0x20).collect();
    m.write_physical(0xFFFF_FFF0, &data);

    // 0xFFFF_FFF0 is within the BIOS ROM alias window, so writes there must be ignored.
    assert_eq!(m.read_physical_bytes(0xFFFF_FFF0, 0x10), rom_before);

    // The bytes that landed at/above 4GiB should be written into RAM.
    assert_eq!(
        m.read_physical_bytes(0x1_0000_0000, 0x10),
        data[0x10..].to_vec()
    );
}
