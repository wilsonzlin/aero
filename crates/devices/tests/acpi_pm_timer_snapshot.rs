use aero_devices::acpi_pm::{
    register_acpi_pm, AcpiPmCallbacks, AcpiPmConfig, AcpiPmIo, PM1_STS_PWRBTN,
};
use aero_devices::irq::IrqLine;
use aero_io_snapshot::io::state::IoSnapshot;
use aero_platform::io::IoPortBus;
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Clone)]
struct TestIrqLine(Rc<RefCell<Vec<bool>>>);

impl IrqLine for TestIrqLine {
    fn set_level(&self, level: bool) {
        self.0.borrow_mut().push(level);
    }
}

#[test]
fn pm_tmr_advances_with_tick() {
    let cfg = AcpiPmConfig::default();
    let pm = Rc::new(RefCell::new(AcpiPmIo::new(cfg)));

    let mut bus = IoPortBus::new();
    register_acpi_pm(&mut bus, pm.clone());

    let t0 = bus.read(cfg.pm_tmr_blk, 4);
    assert_eq!(t0, 0, "PM_TMR must start at 0 for deterministic devices");

    // 1 second at 3.579545MHz.
    pm.borrow_mut().tick(1_000_000_000);
    let t1 = bus.read(cfg.pm_tmr_blk, 4);
    assert_eq!(t1, 3_579_545);
    assert_ne!(t0, t1);
}

#[test]
fn snapshot_roundtrip_restores_pm_timer_and_registers() {
    let cfg = AcpiPmConfig::default();

    let sci_log_1 = Rc::new(RefCell::new(Vec::new()));
    let callbacks_1 = AcpiPmCallbacks {
        sci_irq: Box::new(TestIrqLine(sci_log_1.clone())),
        request_power_off: None,
    };

    let pm1 = Rc::new(RefCell::new(AcpiPmIo::new_with_callbacks(cfg, callbacks_1)));
    let mut bus1 = IoPortBus::new();
    register_acpi_pm(&mut bus1, pm1.clone());

    // Create an SCI-asserting state:
    // - enable PM1 power button
    // - set PM1_STS.PWRBTN_STS
    // - enable ACPI (SCI_EN=1)
    bus1.write(cfg.pm1a_evt_blk + 2, 2, u32::from(PM1_STS_PWRBTN));
    pm1.borrow_mut().trigger_power_button();
    bus1.write(cfg.smi_cmd_port, 1, u32::from(cfg.acpi_enable_cmd));

    // Also exercise GPE0 snapshot fields.
    let gpe0_enable_base = cfg.gpe0_blk + u16::from(cfg.gpe0_blk_len) / 2;
    bus1.write(gpe0_enable_base, 1, 0xAA);
    pm1.borrow_mut().trigger_gpe0(0, 0x55);

    assert!(pm1.borrow().sci_level());
    assert_eq!(sci_log_1.borrow().as_slice(), &[true]);

    pm1.borrow_mut().tick(500_000_000);
    let pm_tmr_before = bus1.read(cfg.pm_tmr_blk, 4);

    let pm1_sts_before = bus1.read(cfg.pm1a_evt_blk, 2) as u16;
    let pm1_en_before = bus1.read(cfg.pm1a_evt_blk + 2, 2) as u16;
    let pm1_cnt_before = bus1.read(cfg.pm1a_cnt_blk, 2) as u16;
    let gpe0_sts_before = bus1.read(cfg.gpe0_blk, 1) as u8;
    let gpe0_en_before = bus1.read(gpe0_enable_base, 1) as u8;

    let snapshot = pm1.borrow().save_state();

    let sci_log_2 = Rc::new(RefCell::new(Vec::new()));
    let callbacks_2 = AcpiPmCallbacks {
        sci_irq: Box::new(TestIrqLine(sci_log_2.clone())),
        request_power_off: None,
    };

    let pm2 = Rc::new(RefCell::new(AcpiPmIo::new_with_callbacks(cfg, callbacks_2)));
    let mut bus2 = IoPortBus::new();
    register_acpi_pm(&mut bus2, pm2.clone());

    pm2.borrow_mut().load_state(&snapshot).unwrap();

    assert!(
        pm2.borrow().sci_level(),
        "SCI must be recomputed and re-driven after restore"
    );
    assert_eq!(
        sci_log_2.borrow().last().copied(),
        Some(true),
        "restored SCI level must be driven onto the IRQ line"
    );

    assert_eq!(bus2.read(cfg.pm_tmr_blk, 4), pm_tmr_before);
    assert_eq!(bus2.read(cfg.pm1a_evt_blk, 2) as u16, pm1_sts_before);
    assert_eq!(bus2.read(cfg.pm1a_evt_blk + 2, 2) as u16, pm1_en_before);
    assert_eq!(bus2.read(cfg.pm1a_cnt_blk, 2) as u16, pm1_cnt_before);
    assert_eq!(bus2.read(cfg.gpe0_blk, 1) as u8, gpe0_sts_before);
    assert_eq!(bus2.read(gpe0_enable_base, 1) as u8, gpe0_en_before);

    // Further time progression should remain deterministic.
    pm1.borrow_mut().tick(123_456);
    pm2.borrow_mut().tick(123_456);
    assert_eq!(bus1.read(cfg.pm_tmr_blk, 4), bus2.read(cfg.pm_tmr_blk, 4));
}
