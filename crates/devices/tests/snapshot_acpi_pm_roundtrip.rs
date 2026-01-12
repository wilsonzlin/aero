use std::cell::RefCell;
use std::rc::Rc;

use aero_devices::acpi_pm::{AcpiPmCallbacks, AcpiPmConfig, AcpiPmIo, PM1_STS_PWRBTN};
use aero_devices::clock::ManualClock;
use aero_devices::irq::IrqLine;
use aero_io_snapshot::io::state::IoSnapshot;
use aero_platform::io::PortIoDevice;

#[derive(Clone, Default)]
struct IrqLog(Rc<RefCell<Vec<bool>>>);

impl IrqLog {
    fn events(&self) -> Vec<bool> {
        self.0.borrow().clone()
    }
}

impl IrqLine for IrqLog {
    fn set_level(&self, level: bool) {
        self.0.borrow_mut().push(level);
    }
}

#[test]
fn snapshot_bytes_are_deterministic_without_clock_advance() {
    let cfg = AcpiPmConfig::default();
    let clock = ManualClock::new();
    let pm = AcpiPmIo::new_with_callbacks_and_clock(cfg, AcpiPmCallbacks::default(), clock);
    assert_eq!(pm.save_state(), pm.save_state());
}

#[test]
fn snapshot_restore_redrives_sci_level() {
    let cfg = AcpiPmConfig::default();
    let clock = ManualClock::new();

    let irq = IrqLog::default();
    let mut pm = AcpiPmIo::new_with_callbacks_and_clock(
        cfg,
        AcpiPmCallbacks {
            sci_irq: Box::new(irq),
            request_power_off: None,
        },
        clock.clone(),
    );

    // Enable the power button status bit, then enable ACPI (SCI_EN) and trigger the event so the
    // SCI line is asserted.
    pm.write(cfg.pm1a_evt_blk + 2, 2, u32::from(PM1_STS_PWRBTN));
    pm.write(cfg.smi_cmd_port, 1, u32::from(cfg.acpi_enable_cmd));
    pm.trigger_power_button();
    assert!(pm.sci_level());

    let snapshot = pm.save_state();

    let irq2 = IrqLog::default();
    let mut restored = AcpiPmIo::new_with_callbacks_and_clock(
        cfg,
        AcpiPmCallbacks {
            sci_irq: Box::new(irq2.clone()),
            request_power_off: None,
        },
        clock,
    );
    restored.load_state(&snapshot).unwrap();

    assert!(restored.sci_level());
    assert_eq!(
        irq2.events(),
        vec![true],
        "restored device must assert SCI immediately based on restored state"
    );
}

#[test]
fn snapshot_restore_preserves_pm_tmr_phase_and_advances_identically() {
    let cfg = AcpiPmConfig::default();
    let clock = ManualClock::new();
    clock.set_ns(1_000_000_000);

    let mut pm = AcpiPmIo::new_with_callbacks_and_clock(cfg, AcpiPmCallbacks::default(), clock.clone());
    clock.advance_ns(123_456_789);
    let tmr_before = pm.read(cfg.pm_tmr_blk, 4);

    let snapshot = pm.save_state();

    let mut restored = AcpiPmIo::new_with_callbacks_and_clock(
        cfg,
        AcpiPmCallbacks::default(),
        clock.clone(),
    );
    restored.load_state(&snapshot).unwrap();
    let tmr_restored = restored.read(cfg.pm_tmr_blk, 4);
    assert_eq!(tmr_restored, tmr_before);

    clock.advance_ns(1_000_000);
    let tmr_after = pm.read(cfg.pm_tmr_blk, 4);
    let tmr_restored_after = restored.read(cfg.pm_tmr_blk, 4);
    assert_eq!(tmr_restored_after, tmr_after);
    assert_ne!(tmr_after, tmr_before);
}
