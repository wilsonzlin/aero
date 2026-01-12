use aero_devices::acpi_pm::{
    register_acpi_pm, AcpiPmCallbacks, AcpiPmConfig, AcpiPmIo, PM1_STS_PWRBTN, SLP_TYP_S5,
};
use aero_devices::irq::IrqLine;
use aero_io_snapshot::io::state::IoSnapshot;
use aero_platform::io::IoPortBus;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

#[derive(Clone)]
struct TestIrqLine(Rc<RefCell<Vec<bool>>>);

impl IrqLine for TestIrqLine {
    fn set_level(&self, level: bool) {
        self.0.borrow_mut().push(level);
    }
}

#[test]
fn pm1_status_write_one_to_clear_and_sci_level() {
    let cfg = AcpiPmConfig::default();

    let sci_log: Rc<RefCell<Vec<bool>>> = Rc::new(RefCell::new(Vec::new()));

    let callbacks = AcpiPmCallbacks {
        sci_irq: Box::new(TestIrqLine(sci_log.clone())),
        request_power_off: None,
    };

    let pm = Rc::new(RefCell::new(AcpiPmIo::new_with_callbacks(cfg, callbacks)));
    let mut bus = IoPortBus::new();
    register_acpi_pm(&mut bus, pm.clone());

    // Enable PWRBTN, but ACPI is still disabled (SCI_EN=0).
    bus.write(cfg.pm1a_evt_blk + 2, 2, u32::from(PM1_STS_PWRBTN));

    pm.borrow_mut().trigger_power_button();
    assert!(!pm.borrow().sci_level());

    // Standard ACPI enable handshake: write ACPI_ENABLE to SMI_CMD.
    bus.write(cfg.smi_cmd_port, 1, u32::from(cfg.acpi_enable_cmd));
    assert!(pm.borrow().sci_level());
    assert_eq!(sci_log.borrow().as_slice(), &[true]);

    // Clear PWRBTN_STS -> SCI should deassert.
    bus.write(cfg.pm1a_evt_blk, 2, u32::from(PM1_STS_PWRBTN));
    assert!(!pm.borrow().sci_level());
    assert_eq!(sci_log.borrow().as_slice(), &[true, false]);
}

#[test]
fn s5_sleep_requests_poweroff() {
    let cfg = AcpiPmConfig::default();

    let power_off_count = Rc::new(Cell::new(0u32));
    let power_off_cb = power_off_count.clone();

    let callbacks = AcpiPmCallbacks {
        request_power_off: Some(Box::new(move || power_off_cb.set(power_off_cb.get() + 1))),
        ..Default::default()
    };

    let pm = Rc::new(RefCell::new(AcpiPmIo::new_with_callbacks(cfg, callbacks)));
    let mut bus = IoPortBus::new();
    register_acpi_pm(&mut bus, pm);

    let pm1_cnt = ((SLP_TYP_S5 as u32) << 10) | (1u32 << 13);
    bus.write(cfg.pm1a_cnt_blk, 2, pm1_cnt);
    assert_eq!(power_off_count.get(), 1);
}

#[test]
fn pm_tmr_is_deterministic_and_wraps_24bit() {
    const PM_TIMER_HZ: u64 = 3_579_545;
    const PM_TIMER_MASK_24BIT: u32 = 0x00FF_FFFF;

    let cfg = AcpiPmConfig::default();

    // Two sub-tick updates should accumulate and match a single larger update.
    let pm_a = Rc::new(RefCell::new(AcpiPmIo::new(cfg)));
    let mut bus_a = IoPortBus::new();
    register_acpi_pm(&mut bus_a, pm_a.clone());

    pm_a.borrow_mut().advance_ns(279);
    pm_a.borrow_mut().advance_ns(279);
    let v_a = bus_a.read(cfg.pm_tmr_blk, 4);

    let pm_b = Rc::new(RefCell::new(AcpiPmIo::new(cfg)));
    let mut bus_b = IoPortBus::new();
    register_acpi_pm(&mut bus_b, pm_b.clone());

    pm_b.borrow_mut().advance_ns(558);
    let v_b = bus_b.read(cfg.pm_tmr_blk, 4);

    assert_eq!(v_a, v_b);
    assert_eq!(v_a & PM_TIMER_MASK_24BIT, 1);

    // Large delta should wrap at 24 bits.
    let pm_wrap = Rc::new(RefCell::new(AcpiPmIo::new(cfg)));
    let mut bus_wrap = IoPortBus::new();
    register_acpi_pm(&mut bus_wrap, pm_wrap.clone());

    pm_wrap.borrow_mut().advance_ns(5_000_000_000); // 5 seconds
    let v_wrap = bus_wrap.read(cfg.pm_tmr_blk, 4);

    let expected = ((PM_TIMER_HZ * 5) as u32) & PM_TIMER_MASK_24BIT;
    assert_eq!(v_wrap & PM_TIMER_MASK_24BIT, expected);
}

#[test]
fn pm_tmr_snapshot_roundtrip_preserves_timer_state() {
    let cfg = AcpiPmConfig::default();

    let pm = Rc::new(RefCell::new(AcpiPmIo::new(cfg)));
    let mut bus = IoPortBus::new();
    register_acpi_pm(&mut bus, pm.clone());

    pm.borrow_mut().advance_ns(279);
    pm.borrow_mut().advance_ns(279);
    let before = bus.read(cfg.pm_tmr_blk, 4);

    let snapshot = pm.borrow().save_state();
    assert_eq!(snapshot, pm.borrow().save_state());

    let restored = Rc::new(RefCell::new(AcpiPmIo::new(cfg)));
    restored.borrow_mut().load_state(&snapshot).unwrap();
    let mut bus2 = IoPortBus::new();
    register_acpi_pm(&mut bus2, restored.clone());

    let after = bus2.read(cfg.pm_tmr_blk, 4);
    assert_eq!(before, after);

    // Subsequent time advances should stay in sync.
    pm.borrow_mut().advance_ns(1_000_000_000);
    restored.borrow_mut().advance_ns(1_000_000_000);
    assert_eq!(bus.read(cfg.pm_tmr_blk, 4), bus2.read(cfg.pm_tmr_blk, 4));
}
