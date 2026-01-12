use std::cell::RefCell;
use std::rc::Rc;

use aero_devices::acpi_pm::{AcpiPmCallbacks, AcpiPmConfig, AcpiPmIo, PM1_STS_PWRBTN};
use aero_devices::clock::ManualClock;
use aero_devices::irq::IrqLine;
use aero_io_snapshot::io::state::{IoSnapshot, SnapshotVersion, SnapshotWriter};
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

    let mut pm =
        AcpiPmIo::new_with_callbacks_and_clock(cfg, AcpiPmCallbacks::default(), clock.clone());
    clock.advance_ns(123_456_789);
    let tmr_before = pm.read(cfg.pm_tmr_blk, 4);

    let snapshot = pm.save_state();

    let mut restored =
        AcpiPmIo::new_with_callbacks_and_clock(cfg, AcpiPmCallbacks::default(), clock.clone());
    restored.load_state(&snapshot).unwrap();
    let tmr_restored = restored.read(cfg.pm_tmr_blk, 4);
    assert_eq!(tmr_restored, tmr_before);

    clock.advance_ns(1_000_000);
    let tmr_after = pm.read(cfg.pm_tmr_blk, 4);
    let tmr_restored_after = restored.read(cfg.pm_tmr_blk, 4);
    assert_eq!(tmr_restored_after, tmr_after);
    assert_ne!(tmr_after, tmr_before);
}

#[test]
fn snapshot_restore_in_place_does_not_glitch_sci_level() {
    let cfg = AcpiPmConfig::default();
    let clock = ManualClock::new();

    let irq = IrqLog::default();
    let mut pm = AcpiPmIo::new_with_callbacks_and_clock(
        cfg,
        AcpiPmCallbacks {
            sci_irq: Box::new(irq.clone()),
            request_power_off: None,
        },
        clock.clone(),
    );

    pm.write(cfg.pm1a_evt_blk + 2, 2, u32::from(PM1_STS_PWRBTN));
    pm.write(cfg.smi_cmd_port, 1, u32::from(cfg.acpi_enable_cmd));
    pm.trigger_power_button();
    assert!(pm.sci_level());

    // Clear the initial assertion edge so we can observe what `load_state` does.
    irq.0.borrow_mut().clear();

    let snapshot = pm.save_state();
    pm.load_state(&snapshot).unwrap();

    assert!(
        irq.events().is_empty(),
        "restoring into an already-asserted instance must not toggle SCI"
    );
    assert!(pm.sci_level());
}

#[test]
fn snapshot_restore_in_place_deasserts_sci_when_snapshot_state_has_no_pending_events() {
    let cfg = AcpiPmConfig::default();
    let clock = ManualClock::new();

    let irq = IrqLog::default();
    let mut pm = AcpiPmIo::new_with_callbacks_and_clock(
        cfg,
        AcpiPmCallbacks {
            sci_irq: Box::new(irq.clone()),
            request_power_off: None,
        },
        clock.clone(),
    );

    // Assert SCI in the live device.
    pm.write(cfg.pm1a_evt_blk + 2, 2, u32::from(PM1_STS_PWRBTN));
    pm.write(cfg.smi_cmd_port, 1, u32::from(cfg.acpi_enable_cmd));
    pm.trigger_power_button();
    assert!(pm.sci_level());

    // Clear the initial assertion edge so we can observe what `load_state` does.
    irq.0.borrow_mut().clear();

    // Restore a minimal/empty snapshot (no fields) which corresponds to the baseline (no pending
    // events, SCI_EN=0).
    let snapshot = SnapshotWriter::new(*b"ACPM", SnapshotVersion::new(1, 0)).finish();
    pm.load_state(&snapshot).unwrap();

    assert_eq!(
        irq.events(),
        vec![false],
        "restoring a snapshot with no pending SCI should deassert the line exactly once"
    );
    assert!(!pm.sci_level());
}

#[test]
fn snapshot_load_ignores_unknown_tags() {
    let cfg = AcpiPmConfig::default();
    let clock0 = ManualClock::new();
    let mut pm0 =
        AcpiPmIo::new_with_callbacks_and_clock(cfg, AcpiPmCallbacks::default(), clock0.clone());

    pm0.write(cfg.pm1a_evt_blk + 2, 2, u32::from(PM1_STS_PWRBTN));
    pm0.write(cfg.smi_cmd_port, 1, u32::from(cfg.acpi_enable_cmd));
    pm0.trigger_power_button();
    assert!(pm0.sci_level());

    let mut snap = pm0.save_state();
    // Append an unknown field tag (forward compatibility).
    snap.extend_from_slice(&0xFFFFu16.to_le_bytes());
    snap.extend_from_slice(&3u32.to_le_bytes());
    snap.extend_from_slice(&[1, 2, 3]);

    let clock1 = ManualClock::new();
    clock1.set_ns(5_000_000);
    let mut pm1 = AcpiPmIo::new_with_callbacks_and_clock(cfg, AcpiPmCallbacks::default(), clock1);
    pm1.load_state(&snap).unwrap();

    assert!(pm1.sci_level());
    assert_eq!(pm1.pm1_status(), PM1_STS_PWRBTN);
}

#[test]
fn snapshot_load_tolerates_gpe0_length_mismatch_for_forward_compat() {
    let cfg = AcpiPmConfig::default();
    let half = (cfg.gpe0_blk_len as usize) / 2;

    // Build snapshots with mismatched-length GPE0 fields and ensure they still restore.
    const TAG_GPE0_STS: u16 = 4;
    const TAG_GPE0_EN: u16 = 5;
    let mut w = SnapshotWriter::new(*b"ACPM", SnapshotVersion::new(1, 0));
    w.field_bytes(TAG_GPE0_STS, vec![0xAA, 0xBB, 0xCC, 0xDD, 0xEE]);
    w.field_bytes(TAG_GPE0_EN, vec![0x11, 0x22, 0x33, 0x44, 0x55]);
    let bytes = w.finish();

    let clock = ManualClock::new();
    let mut pm = AcpiPmIo::new_with_callbacks_and_clock(cfg, AcpiPmCallbacks::default(), clock);
    pm.load_state(&bytes).unwrap();

    // The device's GPE0 status array is `half` bytes long; it should load the prefix and ignore
    // the extra trailing byte.
    if half >= 1 {
        assert_eq!(pm.read(cfg.gpe0_blk, 1) as u8, 0xAA);
    }
    if half >= 2 {
        assert_eq!(pm.read(cfg.gpe0_blk + 1, 1) as u8, 0xBB);
    }
    if half >= 3 {
        assert_eq!(pm.read(cfg.gpe0_blk + 2, 1) as u8, 0xCC);
    }
    if half >= 4 {
        assert_eq!(pm.read(cfg.gpe0_blk + 3, 1) as u8, 0xDD);
    }

    if half >= 1 {
        assert_eq!(pm.read(cfg.gpe0_blk + half as u16, 1) as u8, 0x11);
    }
    if half >= 2 {
        assert_eq!(pm.read(cfg.gpe0_blk + half as u16 + 1, 1) as u8, 0x22);
    }
    if half >= 3 {
        assert_eq!(pm.read(cfg.gpe0_blk + half as u16 + 2, 1) as u8, 0x33);
    }
    if half >= 4 {
        assert_eq!(pm.read(cfg.gpe0_blk + half as u16 + 3, 1) as u8, 0x44);
    }
}

#[test]
fn snapshot_load_short_gpe0_fields_clear_remaining_bytes() {
    let cfg = AcpiPmConfig::default();
    let half = (cfg.gpe0_blk_len as usize) / 2;

    let clock = ManualClock::new();
    let mut pm = AcpiPmIo::new_with_callbacks_and_clock(cfg, AcpiPmCallbacks::default(), clock);

    // Pre-fill GPE0 state so we can verify that bytes beyond the short snapshot payload are
    // cleared back to baseline (0) instead of leaking the old state.
    for i in 0..half {
        pm.trigger_gpe0(i, 0xFF);
        pm.write(cfg.gpe0_blk + half as u16 + i as u16, 1, 0xFF);
    }

    // Restore from a snapshot that only contains 1 byte of GPE0_STS and GPE0_EN.
    const TAG_GPE0_STS: u16 = 4;
    const TAG_GPE0_EN: u16 = 5;
    let mut w = SnapshotWriter::new(*b"ACPM", SnapshotVersion::new(1, 0));
    w.field_bytes(TAG_GPE0_STS, vec![0xAA]);
    w.field_bytes(TAG_GPE0_EN, vec![0x11]);
    pm.load_state(&w.finish()).unwrap();

    if half == 0 {
        return;
    }

    assert_eq!(pm.read(cfg.gpe0_blk, 1) as u8, 0xAA);
    assert_eq!(pm.read(cfg.gpe0_blk + half as u16, 1) as u8, 0x11);

    for i in 1..half {
        assert_eq!(pm.read(cfg.gpe0_blk + i as u16, 1) as u8, 0);
        assert_eq!(pm.read(cfg.gpe0_blk + half as u16 + i as u16, 1) as u8, 0);
    }
}

#[test]
fn snapshot_load_does_not_trigger_s5_poweroff_callback() {
    // Even if a snapshot encodes a PM1 control register value that would normally request S5,
    // restoring that state must *not* invoke the host power-off callback. The callback should only
    // fire in response to a guest write while the VM is running.
    let cfg = AcpiPmConfig::default();
    let clock = ManualClock::new();

    let power_off_calls = Rc::new(RefCell::new(0u32));
    let power_off_calls_for_cb = power_off_calls.clone();

    let mut pm = AcpiPmIo::new_with_callbacks_and_clock(
        cfg,
        AcpiPmCallbacks {
            sci_irq: Box::new(IrqLog::default()),
            request_power_off: Some(Box::new(move || {
                *power_off_calls_for_cb.borrow_mut() += 1;
            })),
        },
        clock,
    );

    // Build an intentionally contrived snapshot that sets SLP_TYP=S5 and SLP_EN=1.
    const TAG_PM1_CNT: u16 = 3;
    let pm1_cnt = (u16::from(aero_devices::acpi_pm::SLP_TYP_S5) << 10) | (1 << 13);
    let mut w = SnapshotWriter::new(*b"ACPM", SnapshotVersion::new(1, 0));
    w.field_u16(TAG_PM1_CNT, pm1_cnt);
    let bytes = w.finish();

    pm.load_state(&bytes).unwrap();
    assert_eq!(
        *power_off_calls.borrow(),
        0,
        "load_state must not call request_power_off"
    );
}
