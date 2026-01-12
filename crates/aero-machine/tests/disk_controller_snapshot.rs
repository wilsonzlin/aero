use aero_io_snapshot::io::state::IoSnapshot as _;
use aero_io_snapshot::io::storage::state::DiskControllersSnapshot;
use aero_machine::{Machine, MachineConfig};
use aero_snapshot as snapshot;
use pretty_assertions::assert_eq;

#[test]
fn machine_snapshots_disk_controllers_using_dskc_wrapper_keyed_by_packed_pci_bdf() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_ahci: true,
        // Keep this test focused on disk controller snapshot plumbing.
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    };

    let src = Machine::new(cfg.clone()).unwrap();
    let states = snapshot::SnapshotSource::device_states(&src);

    let disk_controller_state = states
        .iter()
        .find(|s| s.id == snapshot::DeviceId::DISK_CONTROLLER)
        .expect("expected DISK_CONTROLLER device state when AHCI is enabled")
        .clone();

    // Outer encoding should use the canonical `DSKC` wrapper.
    assert_eq!(disk_controller_state.version, 1);
    assert_eq!(disk_controller_state.flags, 0);
    assert_eq!(
        disk_controller_state.data.get(8..12),
        Some(b"DSKC".as_slice())
    );

    // Decode and confirm the controller map is keyed by packed PCI BDF (`PciBdf::pack_u16()`).
    let mut wrapper = DiskControllersSnapshot::default();
    snapshot::apply_io_snapshot_to_device(&disk_controller_state, &mut wrapper).unwrap();

    let expected_key = aero_devices::pci::profile::SATA_AHCI_ICH9.bdf.pack_u16();
    assert_eq!(
        expected_key, 0x0010,
        "expected canonical packed BDF for 00:02.0"
    );

    assert_eq!(
        wrapper.controllers().keys().copied().collect::<Vec<_>>(),
        vec![expected_key]
    );

    let nested = wrapper
        .controllers()
        .get(&expected_key)
        .expect("missing AHCI controller entry for canonical BDF")
        .clone();
    assert_eq!(nested.get(8..12), Some(b"AHCP".as_slice()));

    // Restore device states into a fresh machine and ensure the AHCI controller state matches the
    // nested snapshot blob.
    let mut restored = Machine::new(cfg).unwrap();
    snapshot::SnapshotTarget::restore_device_states(&mut restored, states);

    let ahci = restored.ahci().expect("AHCI enabled");
    assert_eq!(ahci.borrow().save_state(), nested);
}

#[test]
fn machine_snapshots_ahci_and_ide_together_in_dskc_wrapper_sorted_by_packed_bdf() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_ahci: true,
        enable_ide: true,
        // Keep this test focused on disk controller snapshot plumbing.
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    };

    let src = Machine::new(cfg.clone()).unwrap();
    let states = snapshot::SnapshotSource::device_states(&src);

    let disk_controller_state = states
        .iter()
        .find(|s| s.id == snapshot::DeviceId::DISK_CONTROLLER)
        .expect("expected DISK_CONTROLLER device state when disk controllers are enabled")
        .clone();
    assert_eq!(
        disk_controller_state.data.get(8..12),
        Some(b"DSKC".as_slice())
    );

    let mut wrapper = DiskControllersSnapshot::default();
    snapshot::apply_io_snapshot_to_device(&disk_controller_state, &mut wrapper).unwrap();

    let ide_key = aero_devices::pci::profile::IDE_PIIX3.bdf.pack_u16();
    let ahci_key = aero_devices::pci::profile::SATA_AHCI_ICH9.bdf.pack_u16();
    assert_eq!(ide_key, 0x0009, "expected canonical packed BDF for 00:01.1");
    assert_eq!(
        ahci_key, 0x0010,
        "expected canonical packed BDF for 00:02.0"
    );
    assert_eq!(
        wrapper.controllers().keys().copied().collect::<Vec<_>>(),
        vec![ide_key, ahci_key]
    );

    let nested_ide = wrapper
        .controllers()
        .get(&ide_key)
        .expect("missing IDE controller entry for canonical BDF")
        .clone();
    let nested_ahci = wrapper
        .controllers()
        .get(&ahci_key)
        .expect("missing AHCI controller entry for canonical BDF")
        .clone();
    assert_eq!(nested_ide.get(8..12), Some(b"IDE0".as_slice()));
    assert_eq!(nested_ahci.get(8..12), Some(b"AHCP".as_slice()));

    // Roundtrip through the machine restore plumbing and confirm the nested controller blobs were
    // applied to the correct device models.
    let mut restored = Machine::new(cfg).unwrap();
    snapshot::SnapshotTarget::restore_device_states(&mut restored, states);

    let restored_ide = restored.ide().expect("IDE enabled");
    let restored_ahci = restored.ahci().expect("AHCI enabled");
    assert_eq!(restored_ide.borrow().save_state(), nested_ide);
    assert_eq!(restored_ahci.borrow().save_state(), nested_ahci);
}
