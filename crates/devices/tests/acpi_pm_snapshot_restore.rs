use aero_devices::acpi_pm::{
    AcpiPmCallbacks, AcpiPmConfig, AcpiPmIo, AcpiSleepState, SLP_TYP_S3, SLP_TYP_S5,
};
use aero_io_snapshot::io::state::{IoSnapshot, SnapshotVersion, SnapshotWriter};
use aero_platform::io::PortIoDevice;
use std::cell::Cell;
use std::rc::Rc;

#[test]
fn snapshot_restore_does_not_trigger_s5_poweroff_callback() {
    let cfg = AcpiPmConfig::default();

    // Build a snapshot that encodes an S5 request value in PM1a_CNT. The device model may clear
    // SLP_EN after latching the request, so construct the snapshot directly instead of relying on
    // guest writes to keep the bit set.
    const TAG_PM1_CNT: u16 = 3;
    let pm1_cnt = ((SLP_TYP_S5 as u16) << 10) | (1u16 << 13);
    let mut w = SnapshotWriter::new(*b"ACPM", SnapshotVersion::new(1, 0));
    w.field_u16(TAG_PM1_CNT, pm1_cnt);
    let snap = w.finish();

    // Restoring must not replay guest writes or trigger host callbacks.
    let power_off_count = Rc::new(Cell::new(0u32));
    let power_off_cb = power_off_count.clone();
    let callbacks = AcpiPmCallbacks {
        request_power_off: Some(Box::new(move || power_off_cb.set(power_off_cb.get() + 1))),
        request_sleep: None,
        ..Default::default()
    };
    let mut restored = AcpiPmIo::new_with_callbacks(cfg, callbacks);
    restored.load_state(&snap).unwrap();

    assert_eq!(
        power_off_count.get(),
        0,
        "load_state must be side-effect free"
    );
    assert_eq!(restored.pm1_cnt(), pm1_cnt);

    // Prove the callback wiring still works for subsequent guest writes.
    restored.write(cfg.pm1a_cnt_blk + 1, 1, 0); // clear SLP_TYP/SLP_EN
    assert_eq!(power_off_count.get(), 0);
    restored.write(cfg.pm1a_cnt_blk + 1, 1, u32::from((pm1_cnt >> 8) & 0xFF));
    assert_eq!(power_off_count.get(), 1);
}

#[test]
fn snapshot_restore_does_not_trigger_sleep_callback() {
    let cfg = AcpiPmConfig::default();

    // Snapshot with an S3 request in PM1a_CNT.
    const TAG_PM1_CNT: u16 = 3;
    let pm1_cnt = ((SLP_TYP_S3 as u16) << 10) | (1u16 << 13);
    let mut w = SnapshotWriter::new(*b"ACPM", SnapshotVersion::new(1, 0));
    w.field_u16(TAG_PM1_CNT, pm1_cnt);
    let snap = w.finish();

    let sleep_count = Rc::new(Cell::new(0u32));
    let sleep_count_for_cb = sleep_count.clone();
    let callbacks = AcpiPmCallbacks {
        request_sleep: Some(Box::new(move |state| {
            assert_eq!(state, AcpiSleepState::S3);
            sleep_count_for_cb.set(sleep_count_for_cb.get() + 1);
        })),
        request_power_off: None,
        ..Default::default()
    };

    let mut restored = AcpiPmIo::new_with_callbacks(cfg, callbacks);
    restored.load_state(&snap).unwrap();

    assert_eq!(sleep_count.get(), 0, "load_state must be side-effect free");
    assert_eq!(restored.pm1_cnt(), pm1_cnt);
}
