use aero_devices::pci::{PciIntxRouter, PciIntxRouterConfig};
use aero_io_snapshot::io::state::codec::Encoder;
use aero_io_snapshot::io::state::{IoSnapshot, SnapshotError, SnapshotWriter};

#[test]
fn pci_intx_router_snapshot_rejects_excessive_source_count() {
    const TAG_SOURCES: u16 = 2;
    const MAX_INTX_SOURCES: u32 = 256 * 32 * 8 * 4;

    let sources = Encoder::new().u32(MAX_INTX_SOURCES + 1).finish();

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

