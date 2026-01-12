use aero_devices::pci::{profile, PciBdf};
use aero_io_snapshot::io::state::IoSnapshot as _;
use aero_io_snapshot::io::storage::state::{AhciControllerState, DiskControllersSnapshot};

#[test]
fn dskc_controller_map_keys_use_pci_bdf_packed_u16_layout() {
    // Canonical AHCI controller BDF is 00:02.0.
    let bdf = profile::SATA_AHCI_ICH9.bdf;

    // The packed layout is the standard PCI config-address BDF encoding:
    // (bus<<8) | (device<<3) | function.
    let packed = bdf.pack_u16();
    assert_eq!(packed, 0x0010);
    assert_eq!(PciBdf::unpack_u16(packed), bdf);

    // The DSKC wrapper stores nested controller snapshots keyed by this packed u16.
    let ahci_state = AhciControllerState::default().save_state();
    let mut controllers = DiskControllersSnapshot::new();
    controllers.insert(packed, ahci_state.clone());

    let bytes = controllers.save_state();

    let mut restored = DiskControllersSnapshot::default();
    restored
        .load_state(&bytes)
        .expect("DiskControllersSnapshot should decode");

    assert_eq!(
        restored.controllers().get(&packed),
        Some(&ahci_state),
        "controller snapshot bytes should roundtrip under packed BDF key"
    );
}
