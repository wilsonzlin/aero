use aero_devices::pci::PciBusSnapshot;
use aero_io_snapshot::io::state::codec::Encoder;
use aero_io_snapshot::io::state::{IoSnapshot, SnapshotError, SnapshotWriter};

#[test]
fn pci_bus_snapshot_rejects_excessive_bdf_count() {
    const TAG_DEVICES: u16 = 1;
    const MAX_PCI_FUNCTIONS: u32 = 256 * 32 * 8;

    let devices = Encoder::new().u32(MAX_PCI_FUNCTIONS + 1).finish();

    let mut w = SnapshotWriter::new(PciBusSnapshot::DEVICE_ID, PciBusSnapshot::DEVICE_VERSION);
    w.field_bytes(TAG_DEVICES, devices);
    let bytes = w.finish();

    let mut snapshot = PciBusSnapshot::default();
    let err = snapshot.load_state(&bytes).unwrap_err();
    match err {
        SnapshotError::InvalidFieldEncoding(_) => {}
        other => panic!("expected InvalidFieldEncoding, got {other:?}"),
    }
}

