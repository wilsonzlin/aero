use aero_devices::pci::{PciBdf, PciIntxRouter, PciIntxRouterConfig, PciInterruptPin};
use aero_io_snapshot::io::state::codec::Encoder;
use aero_io_snapshot::io::state::{IoSnapshot, SnapshotError, SnapshotWriter};

#[test]
fn pci_intx_router_snapshot_rejects_duplicate_sources() {
    const TAG_SOURCES: u16 = 2;

    let bdf = PciBdf::new(0, 1, 0);
    let pin = PciInterruptPin::IntA.to_config_u8();

    let sources = Encoder::new()
        .u32(2)
        .u8(bdf.bus)
        .u8(bdf.device)
        .u8(bdf.function)
        .u8(pin)
        .bool(true)
        .u8(bdf.bus)
        .u8(bdf.device)
        .u8(bdf.function)
        .u8(pin)
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

