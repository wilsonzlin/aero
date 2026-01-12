use aero_devices::acpi_pm::{AcpiPmCallbacks, AcpiPmConfig, AcpiPmIo, SLP_TYP_S5};
use aero_io_snapshot::io::state::IoSnapshot;
use aero_platform::io::PortIoDevice;
use std::cell::Cell;
use std::rc::Rc;

#[test]
fn snapshot_restore_does_not_trigger_s5_poweroff_callback() {
    let cfg = AcpiPmConfig::default();

    // Program PM1a_CNT to an S5 request value via guest I/O writes so it is present in the
    // serialized state.
    let mut src = AcpiPmIo::new_with_callbacks(cfg, AcpiPmCallbacks::default());
    let pm1_cnt = ((SLP_TYP_S5 as u32) << 10) | (1u32 << 13);
    src.write(cfg.pm1a_cnt_blk, 2, pm1_cnt);
    let snap = src.save_state();

    // Restoring must not replay guest writes or trigger host callbacks.
    let power_off_count = Rc::new(Cell::new(0u32));
    let power_off_cb = power_off_count.clone();
    let callbacks = AcpiPmCallbacks {
        request_power_off: Some(Box::new(move || power_off_cb.set(power_off_cb.get() + 1))),
        ..Default::default()
    };
    let mut restored = AcpiPmIo::new_with_callbacks(cfg, callbacks);
    restored.load_state(&snap).unwrap();

    assert_eq!(
        power_off_count.get(),
        0,
        "load_state must be side-effect free"
    );
    assert_eq!(restored.pm1_cnt(), pm1_cnt as u16);

    // Prove the callback wiring still works for subsequent guest writes.
    restored.write(cfg.pm1a_cnt_blk + 1, 1, 0); // clear SLP_TYP/SLP_EN
    assert_eq!(power_off_count.get(), 0);
    restored.write(cfg.pm1a_cnt_blk + 1, 1, (pm1_cnt >> 8) & 0xFF);
    assert_eq!(power_off_count.get(), 1);
}

