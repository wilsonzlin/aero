use aero_devices::acpi_pm::{register_acpi_pm, AcpiPmCallbacks, AcpiPmConfig, AcpiPmIo, PM1_STS_PWRBTN, SLP_TYP_S5};
use aero_devices::irq::IrqLine;
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
