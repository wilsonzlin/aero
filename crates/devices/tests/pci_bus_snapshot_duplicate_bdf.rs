use aero_devices::pci::{PciBusSnapshot, PciBdf};
use aero_io_snapshot::io::state::codec::Encoder;
use aero_io_snapshot::io::state::{IoSnapshot, SnapshotError, SnapshotWriter};

#[test]
fn pci_bus_snapshot_rejects_duplicate_bdf_entries() {
    const TAG_DEVICES: u16 = 1;

    let bdf = PciBdf::new(0, 1, 0);

    let mut enc = Encoder::new().u32(2);
    for _ in 0..2 {
        enc = enc
            .u8(bdf.bus)
            .u8(bdf.device)
            .u8(bdf.function)
            // config bytes
            .bytes(&[0u8; 256])
            // bar_base + bar_probe for 6 BARs
            .u64(0)
            .bool(false)
            .u64(0)
            .bool(false)
            .u64(0)
            .bool(false)
            .u64(0)
            .bool(false)
            .u64(0)
            .bool(false)
            .u64(0)
            .bool(false);
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

