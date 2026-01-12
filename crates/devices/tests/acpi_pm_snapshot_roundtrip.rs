use aero_devices::acpi_pm::{AcpiPmCallbacks, AcpiPmConfig, AcpiPmIo, PM1_STS_PWRBTN, SLP_TYP_S5};
use aero_devices::clock::ManualClock;
use aero_devices::irq::IrqLine;
use aero_io_snapshot::io::state::IoSnapshot;
use aero_platform::io::PortIoDevice;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

const PM_TIMER_MASK_24BIT: u32 = 0x00FF_FFFF;

#[derive(Clone)]
struct RecordingIrqLine {
    level: Rc<Cell<bool>>,
    events: Rc<RefCell<Vec<bool>>>,
}

impl IrqLine for RecordingIrqLine {
    fn set_level(&self, level: bool) {
        self.level.set(level);
        self.events.borrow_mut().push(level);
    }
}

#[test]
fn acpi_pm_snapshot_roundtrip_preserves_registers_sci_and_timer() {
    let cfg = AcpiPmConfig::default();
    let half = (cfg.gpe0_blk_len as usize) / 2;

    let clock0 = ManualClock::new();
    let mut pm =
        AcpiPmIo::new_with_callbacks_and_clock(cfg, AcpiPmCallbacks::default(), clock0.clone());
    clock0.set_ns(1_000_000);

    // Program some non-zero GPE state (ensure sts&en==0 so SCI is controlled by PM1).
    let mut gpe_sts = vec![0u8; half];
    let mut gpe_en = vec![0u8; half];
    for i in 0..half {
        gpe_sts[i] = 1u8 << (i.min(3));
        gpe_en[i] = 1u8 << (4 + (i.min(3)));
    }

    for (i, &v) in gpe_en.iter().enumerate() {
        pm.write(cfg.gpe0_blk + half as u16 + i as u16, 1, u32::from(v));
    }
    for (i, &v) in gpe_sts.iter().enumerate() {
        pm.trigger_gpe0(i, v);
    }

    // Enable the PWRBTN event and latch it in PM1_STS.
    pm.write(cfg.pm1a_evt_blk + 2, 2, u32::from(PM1_STS_PWRBTN));
    pm.trigger_power_button();
    assert!(!pm.sci_level(), "SCI should be gated until ACPI is enabled");

    // Standard ACPI enable handshake (sets PM1_CNT.SCI_EN).
    pm.write(cfg.smi_cmd_port, 1, u32::from(cfg.acpi_enable_cmd));
    assert!(
        pm.sci_level(),
        "PM1 event should assert SCI once SCI_EN is set"
    );

    // Program S5 sleep bits so a buggy restore path that replays port writes would spuriously
    // invoke the power-off callback.
    let pm1_cnt = ((SLP_TYP_S5 as u32) << 10) | (1u32 << 13) | 1;
    pm.write(cfg.pm1a_cnt_blk, 2, pm1_cnt);

    let t0 = pm.read(cfg.pm_tmr_blk, 4) & PM_TIMER_MASK_24BIT;

    let snap = pm.save_state();

    // Restore into a fresh device instance with a wired SCI line and power-off callback.
    let sci_level = Rc::new(Cell::new(false));
    let sci_events: Rc<RefCell<Vec<bool>>> = Rc::new(RefCell::new(Vec::new()));
    let irq = RecordingIrqLine {
        level: sci_level.clone(),
        events: sci_events.clone(),
    };

    let power_off_count = Rc::new(Cell::new(0u32));
    let power_off_cb = power_off_count.clone();

    let callbacks = AcpiPmCallbacks {
        sci_irq: Box::new(irq),
        request_power_off: Some(Box::new(move || power_off_cb.set(power_off_cb.get() + 1))),
    };

    let clock1 = ManualClock::new();
    let mut restored = AcpiPmIo::new_with_callbacks_and_clock(cfg, callbacks, clock1.clone());
    // Use a different clock origin to ensure restore re-anchors the PM timer.
    clock1.set_ns(9_000_000);
    restored.load_state(&snap).unwrap();

    assert_eq!(
        power_off_count.get(),
        0,
        "snapshot restore must not trigger host power-off callbacks"
    );

    // Timer continuity: the first read after restore should match the snapshotted value.
    let t1 = restored.read(cfg.pm_tmr_blk, 4) & PM_TIMER_MASK_24BIT;
    assert_eq!(t0, t1);

    // Register round-trip.
    assert_eq!(restored.pm1_cnt(), pm1_cnt as u16);
    assert_eq!(restored.pm1_status(), PM1_STS_PWRBTN);
    let pm1_en = restored.read(cfg.pm1a_evt_blk + 2, 2) as u16;
    assert_eq!(pm1_en, PM1_STS_PWRBTN);

    for (i, &v) in gpe_sts.iter().enumerate() {
        assert_eq!(
            restored.read(cfg.gpe0_blk + i as u16, 1) as u8,
            v,
            "GPE0_STS byte {}",
            i
        );
    }
    for (i, &v) in gpe_en.iter().enumerate() {
        assert_eq!(
            restored.read(cfg.gpe0_blk + half as u16 + i as u16, 1) as u8,
            v,
            "GPE0_EN byte {}",
            i
        );
    }

    // SCI line should have been re-driven based on the restored state.
    assert!(restored.sci_level());
    assert!(sci_level.get());
    assert_eq!(sci_events.borrow().as_slice(), &[true]);

    // Clearing PM1_STS should deassert SCI.
    restored.write(cfg.pm1a_evt_blk, 2, u32::from(PM1_STS_PWRBTN));
    assert!(!restored.sci_level());
    assert!(!sci_level.get());
    assert_eq!(sci_events.borrow().as_slice(), &[true, false]);
}
