use aero_devices::acpi_pm::{
    register_acpi_pm, AcpiPmCallbacks, AcpiPmConfig, AcpiPmIo, PM1_STS_PWRBTN, SLP_TYP_S5,
};
use aero_devices::clock::ManualClock;
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

#[derive(Clone)]
struct TestIrqLevel(Rc<Cell<bool>>);

impl TestIrqLevel {
    fn new() -> Self {
        Self(Rc::new(Cell::new(false)))
    }

    fn level(&self) -> bool {
        self.0.get()
    }
}

impl IrqLine for TestIrqLevel {
    fn set_level(&self, level: bool) {
        self.0.set(level);
    }
}

#[test]
fn pm1_status_write_one_to_clear_and_sci_level() {
    let cfg = AcpiPmConfig::default();
    let clock = ManualClock::new();

    let sci_log: Rc<RefCell<Vec<bool>>> = Rc::new(RefCell::new(Vec::new()));

    let callbacks = AcpiPmCallbacks {
        sci_irq: Box::new(TestIrqLine(sci_log.clone())),
        request_power_off: None,
    };

    let pm = Rc::new(RefCell::new(AcpiPmIo::new_with_callbacks_and_clock(
        cfg, callbacks, clock,
    )));
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
    let clock = ManualClock::new();

    let power_off_count = Rc::new(Cell::new(0u32));
    let power_off_cb = power_off_count.clone();

    let callbacks = AcpiPmCallbacks {
        request_power_off: Some(Box::new(move || power_off_cb.set(power_off_cb.get() + 1))),
        ..Default::default()
    };

    let pm = Rc::new(RefCell::new(AcpiPmIo::new_with_callbacks_and_clock(
        cfg, callbacks, clock,
    )));
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

#[test]
fn snapshot_roundtrip_preserves_pm1_gpe_sci_and_pm_timer_deterministically() {
    let cfg = AcpiPmConfig::default();
    let gpe_half = u16::from(cfg.gpe0_blk_len) / 2;

    let irq0 = TestIrqLevel::new();
    let callbacks0 = AcpiPmCallbacks {
        sci_irq: Box::new(irq0.clone()),
        request_power_off: None,
    };

    let pm0 = Rc::new(RefCell::new(AcpiPmIo::new_with_callbacks(cfg, callbacks0)));
    let mut bus0 = IoPortBus::new();
    register_acpi_pm(&mut bus0, pm0.clone());

    // Enable PWRBTN in PM1_EN and a sample GPE0 bit.
    bus0.write(cfg.pm1a_evt_blk + 2, 2, u32::from(PM1_STS_PWRBTN));
    bus0.write(cfg.gpe0_blk + gpe_half, 1, 0x01);

    pm0.borrow_mut().trigger_power_button();
    pm0.borrow_mut().trigger_gpe0(0, 0x01);
    assert!(
        !pm0.borrow().sci_level(),
        "SCI must remain deasserted until ACPI is enabled"
    );

    // ACPI enable handshake: write ACPI_ENABLE to SMI_CMD.
    bus0.write(cfg.smi_cmd_port, 1, u32::from(cfg.acpi_enable_cmd));
    assert!(pm0.borrow().sci_level());
    assert!(irq0.level());

    // Advance PM_TMR to a non-zero value.
    pm0.borrow_mut().advance_ns(1_000_000);
    let tmr0 = bus0.read(cfg.pm_tmr_blk, 4);
    assert_ne!(tmr0 & 0x00FF_FFFF, 0, "PM_TMR should advance once ticked");

    // Snapshot bytes must be deterministic for a fixed device state.
    let snap1 = pm0.borrow().save_state();
    let snap2 = pm0.borrow().save_state();
    assert_eq!(snap1, snap2);

    // Restore into a fresh device with a fresh IRQ line (starts deasserted).
    let irq1 = TestIrqLevel::new();
    let callbacks1 = AcpiPmCallbacks {
        sci_irq: Box::new(irq1.clone()),
        request_power_off: None,
    };
    let pm1 = Rc::new(RefCell::new(AcpiPmIo::new_with_callbacks(cfg, callbacks1)));
    pm1.borrow_mut().load_state(&snap1).unwrap();

    let mut bus1 = IoPortBus::new();
    register_acpi_pm(&mut bus1, pm1.clone());

    assert!(
        irq1.level(),
        "load_state should re-drive SCI based on pending PM1/GPE bits"
    );

    // Guest-visible registers should match across snapshot/restore.
    assert_eq!(bus0.read(cfg.pm1a_evt_blk, 2), bus1.read(cfg.pm1a_evt_blk, 2)); // PM1_STS
    assert_eq!(
        bus0.read(cfg.pm1a_evt_blk + 2, 2),
        bus1.read(cfg.pm1a_evt_blk + 2, 2)
    ); // PM1_EN
    assert_eq!(bus0.read(cfg.pm1a_cnt_blk, 2), bus1.read(cfg.pm1a_cnt_blk, 2)); // PM1_CNT

    for i in 0..gpe_half {
        assert_eq!(
            bus0.read(cfg.gpe0_blk + i, 1),
            bus1.read(cfg.gpe0_blk + i, 1),
            "GPE0_STS byte {} mismatch",
            i
        );
        assert_eq!(
            bus0.read(cfg.gpe0_blk + gpe_half + i, 1),
            bus1.read(cfg.gpe0_blk + gpe_half + i, 1),
            "GPE0_EN byte {} mismatch",
            i
        );
    }

    // PM_TMR should continue from the same point after restore.
    assert_eq!(bus0.read(cfg.pm_tmr_blk, 4), bus1.read(cfg.pm_tmr_blk, 4));

    // Clearing PM1_STS should not deassert SCI while a GPE remains pending.
    bus0.write(cfg.pm1a_evt_blk, 2, u32::from(PM1_STS_PWRBTN));
    bus1.write(cfg.pm1a_evt_blk, 2, u32::from(PM1_STS_PWRBTN));
    assert!(irq0.level());
    assert!(irq1.level());

    // Clearing GPE0_STS should deassert SCI once no other events are pending.
    bus0.write(cfg.gpe0_blk, 1, 0x01);
    bus1.write(cfg.gpe0_blk, 1, 0x01);
    assert!(!irq0.level());
    assert!(!irq1.level());

    // PM timer must advance deterministically after restore.
    pm0.borrow_mut().advance_ns(123_456_789);
    pm1.borrow_mut().advance_ns(123_456_789);
    assert_eq!(bus0.read(cfg.pm_tmr_blk, 4), bus1.read(cfg.pm_tmr_blk, 4));
}
