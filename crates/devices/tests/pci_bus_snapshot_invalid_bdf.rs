use aero_devices::pci::PciBusSnapshot;
use aero_io_snapshot::io::state::codec::Encoder;
use aero_io_snapshot::io::state::{IoSnapshot, SnapshotError, SnapshotWriter};

#[test]
fn pci_bus_snapshot_rejects_invalid_bdf_encoding() {
    const TAG_DEVICES: u16 = 1;

    // Encode a device number outside the PCI range (device >= 32).
    let mut enc = Encoder::new()
        .u32(1)
        .u8(0) // bus
        .u8(32) // invalid device number
        .u8(0) // function
        .bytes(&[0u8; 256]);

    for _ in 0..6 {
        enc = enc.u64(0).bool(false);
    }

    let mut w = SnapshotWriter::new(PciBusSnapshot::DEVICE_ID, PciBusSnapshot::DEVICE_VERSION);
    w.field_bytes(TAG_DEVICES, enc.finish());
    let bytes = w.finish();

    let mut snapshot = PciBusSnapshot::default();
    let err = snapshot.load_state(&bytes).unwrap_err();
    match err {
        SnapshotError::InvalidFieldEncoding(_) => {}
        other => panic!("expected InvalidFieldEncoding, got {other:?}"),
    }
}

