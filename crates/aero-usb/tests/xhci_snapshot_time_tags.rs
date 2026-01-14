use aero_io_snapshot::io::state::{IoSnapshot, SnapshotReader, SnapshotVersion, SnapshotWriter};
use aero_usb::xhci::XhciController;

// Keep in sync with `crates/aero-usb/src/xhci/snapshot.rs`.
const TAG_TIME_MS: u16 = 27;
const TAG_LAST_TICK_DMA_DWORD: u16 = 28;

#[test]
fn xhci_snapshot_load_accepts_v0_7_last_tick_encoded_under_tag_time_ms() {
    // A short-lived xHCI snapshot v0.7 build accidentally wrote `last_tick_dma_dword` under the
    // `TAG_TIME_MS` tag (27) as a `u32`. Ensure newer versions can still restore such snapshots.
    let mut w = SnapshotWriter::new(*b"XHCI", SnapshotVersion::new(0, 7));
    w.field_u32(TAG_TIME_MS, 0x1234_5678);
    let bytes = w.finish();

    let mut ctrl = XhciController::new();
    ctrl.load_state(&bytes)
        .expect("expected legacy v0.7 snapshot to load");

    // The restored controller should re-encode the field under its dedicated tag (28).
    let bytes2 = ctrl.save_state();
    let r = SnapshotReader::parse(&bytes2, *b"XHCI").expect("parse restored snapshot");
    assert_eq!(
        r.u32(TAG_LAST_TICK_DMA_DWORD)
            .expect("read last_tick_dma_dword")
            .unwrap_or(0),
        0x1234_5678
    );
    assert_eq!(
        r.u64(TAG_TIME_MS).expect("read time_ms").unwrap_or(0),
        0
    );
}

