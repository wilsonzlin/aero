use aero_devices::pci::{PciIntxRouter, PciIntxRouterConfig};
use aero_io_snapshot::io::state::codec::Encoder;
use aero_io_snapshot::io::state::{IoSnapshot, SnapshotError, SnapshotWriter};

#[test]
fn pci_intx_router_snapshot_rejects_invalid_bdf_encoding() {
    const TAG_SOURCES: u16 = 2;

    // Encode an invalid BDF (device >= 32) in the asserted-source list.
    let sources = Encoder::new()
        .u32(1)
        .u8(0) // bus
        .u8(32) // invalid device
        .u8(0) // function
        .u8(1) // INTA#
        .bool(true)
        .finish();

    let mut w = SnapshotWriter::new(PciIntxRouter::DEVICE_ID, PciIntxRouter::DEVICE_VERSION);
    w.field_bytes(TAG_SOURCES, sources);
    let bytes = w.finish();

    let mut router = PciIntxRouter::new(PciIntxRouterConfig::default());
    let err = router.load_state(&bytes).unwrap_err();
    match err {
        SnapshotError::InvalidFieldEncoding(_) => {}
        other => panic!("expected InvalidFieldEncoding, got {other:?}"),
    }
}

