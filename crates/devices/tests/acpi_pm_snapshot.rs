use std::cell::{Cell, RefCell};
use std::rc::Rc;

use aero_devices::acpi_pm::{AcpiPmCallbacks, AcpiPmConfig, AcpiPmIo, PM1_STS_PWRBTN};
use aero_devices::clock::{Clock, ManualClock};
use aero_devices::irq::IrqLine;
use aero_io_snapshot::io::state::{IoSnapshot, SnapshotReader, SnapshotVersion, SnapshotWriter};
use aero_platform::io::PortIoDevice;

#[derive(Clone)]
struct TestIrqLine(Rc<RefCell<Vec<bool>>>);

impl IrqLine for TestIrqLine {
    fn set_level(&self, level: bool) {
        self.0.borrow_mut().push(level);
    }
}

#[test]
fn snapshot_is_deterministic_when_clock_does_not_advance() {
    let cfg = AcpiPmConfig::default();
    let clock = ManualClock::new();
    clock.set_ns(123_456_789);

    let pm = AcpiPmIo::new_with_callbacks_and_clock(cfg, AcpiPmCallbacks::default(), clock);
    assert_eq!(pm.save_state(), pm.save_state());
}

#[test]
fn snapshot_restore_roundtrip_restores_registers_gpe_and_redrives_sci() {
    let cfg = AcpiPmConfig::default();
    let clock0 = ManualClock::new();
    clock0.set_ns(1_000_000);

    let sci_log0: Rc<RefCell<Vec<bool>>> = Rc::new(RefCell::new(Vec::new()));
    let callbacks0 = AcpiPmCallbacks {
        sci_irq: Box::new(TestIrqLine(sci_log0.clone())),
        request_power_off: None,
    };

    let mut pm0 = AcpiPmIo::new_with_callbacks_and_clock(cfg, callbacks0, clock0);

    // Enable power button wake events.
    pm0.write(cfg.pm1a_evt_blk + 2, 2, u32::from(PM1_STS_PWRBTN));
    // Enable ACPI mode (sets SCI_EN).
    pm0.write(cfg.smi_cmd_port, 1, u32::from(cfg.acpi_enable_cmd));
    // Also set some extra bits in PM1_CNT to ensure the full register is restored.
    pm0.write(cfg.pm1a_cnt_blk, 2, 0x1235);

    // Program a GPE enable byte and trigger the corresponding status bit.
    let half = (cfg.gpe0_blk_len as u16) / 2;
    pm0.write(cfg.gpe0_blk + half, 1, 0x02);
    pm0.trigger_gpe0(0, 0x02);

    // Trigger the power button event (sets PM1_STS bit and should assert SCI).
    pm0.trigger_power_button();
    assert!(pm0.sci_level());

    let snap = pm0.save_state();
    assert_eq!(snap, pm0.save_state());

    let clock1 = ManualClock::new();
    // Use a different clock origin to validate that restore re-anchors the PM timer.
    clock1.set_ns(9_000_000);

    let sci_log1: Rc<RefCell<Vec<bool>>> = Rc::new(RefCell::new(Vec::new()));
    let callbacks1 = AcpiPmCallbacks {
        sci_irq: Box::new(TestIrqLine(sci_log1.clone())),
        request_power_off: None,
    };
    let mut pm1 = AcpiPmIo::new_with_callbacks_and_clock(cfg, callbacks1, clock1);
    pm1.load_state(&snap).unwrap();

    assert_eq!(pm1.pm1_status(), PM1_STS_PWRBTN);
    assert_eq!(pm1.read(cfg.pm1a_evt_blk + 2, 2) as u16, PM1_STS_PWRBTN);
    assert_eq!(pm1.pm1_cnt(), 0x1235);

    // GPE0 block: status (first half), enable (second half).
    assert_eq!(pm1.read(cfg.gpe0_blk, 1) as u8, 0x02);
    assert_eq!(pm1.read(cfg.gpe0_blk + half, 1) as u8, 0x02);

    assert!(pm1.sci_level(), "restored device must reassert SCI");
    assert_eq!(sci_log1.borrow().as_slice(), &[true]);
}

#[test]
fn snapshot_restore_preserves_pm_timer_value_and_continuity() {
    let cfg = AcpiPmConfig::default();

    let clock0 = ManualClock::new();
    let mut pm0 =
        AcpiPmIo::new_with_callbacks_and_clock(cfg, AcpiPmCallbacks::default(), clock0.clone());

    clock0.set_ns(1_000_000);
    let t0 = pm0.read(cfg.pm_tmr_blk, 4);

    let snap = pm0.save_state();

    let clock1 = ManualClock::new();
    clock1.set_ns(9_000_000);
    let mut pm1 =
        AcpiPmIo::new_with_callbacks_and_clock(cfg, AcpiPmCallbacks::default(), clock1.clone());
    pm1.load_state(&snap).unwrap();

    let t1 = pm1.read(cfg.pm_tmr_blk, 4);
    assert_eq!(
        t0, t1,
        "PM_TMR must match after restore at a different clock origin"
    );

    clock0.advance_ns(123_456);
    clock1.advance_ns(123_456);
    let t0_after = pm0.read(cfg.pm_tmr_blk, 4);
    let t1_after = pm1.read(cfg.pm_tmr_blk, 4);
    assert_eq!(
        t0_after, t1_after,
        "PM_TMR must advance deterministically after restore"
    );
}

#[test]
fn snapshot_encodes_pm_timer_ticks_and_fractional_remainder() {
    const PM_TIMER_HZ: u128 = 3_579_545;
    const NS_PER_SEC: u128 = 1_000_000_000;

    let cfg = AcpiPmConfig::default();
    let clock = ManualClock::new();
    let pm = AcpiPmIo::new_with_callbacks_and_clock(cfg, AcpiPmCallbacks::default(), clock.clone());

    // Advance by a sub-second delta so we exercise both ticks and remainder.
    clock.advance_ns(1_000);

    let snap = pm.save_state();
    let r = SnapshotReader::parse(&snap, *b"ACPM").unwrap();

    let elapsed_ns = r.u64(6).unwrap().expect("missing elapsed_ns");
    let ticks = r.u32(8).unwrap().expect("missing pm_timer_ticks");
    let remainder = r
        .u32(9)
        .unwrap()
        .expect("missing pm_timer_remainder");

    let numer = (elapsed_ns as u128) * PM_TIMER_HZ;
    let expected_ticks = (numer / NS_PER_SEC) as u32;
    let expected_remainder = (numer % NS_PER_SEC) as u32;

    assert_eq!(ticks, expected_ticks & 0x00FF_FFFF);
    assert_eq!(remainder, expected_remainder);
}

#[derive(Clone)]
struct CountingClock {
    now_ns: Rc<Cell<u64>>,
    calls: Rc<Cell<u32>>,
    step_ns: u64,
}

impl CountingClock {
    fn new(step_ns: u64) -> Self {
        Self {
            now_ns: Rc::new(Cell::new(0)),
            calls: Rc::new(Cell::new(0)),
            step_ns,
        }
    }

    fn calls(&self) -> u32 {
        self.calls.get()
    }

    fn reset_calls(&self) {
        self.calls.set(0);
    }
}

impl Clock for CountingClock {
    fn now_ns(&self) -> u64 {
        let v = self.now_ns.get();
        self.calls.set(self.calls.get().wrapping_add(1));
        self.now_ns.set(v.wrapping_add(self.step_ns));
        v
    }
}

#[test]
fn snapshot_load_samples_clock_once_for_timer_restore() {
    let cfg = AcpiPmConfig::default();
    let clock = CountingClock::new(1_000_000_000);

    let mut pm = AcpiPmIo::new_with_callbacks_and_clock(cfg, AcpiPmCallbacks::default(), clock.clone());
    clock.reset_calls();

    // Normal snapshots: restore via elapsed_ns.
    const TAG_PM_TIMER_ELAPSED_NS: u16 = 6;
    let mut w = SnapshotWriter::new(*b"ACPM", SnapshotVersion::new(1, 0));
    w.field_u64(TAG_PM_TIMER_ELAPSED_NS, 123);
    pm.load_state(&w.finish()).unwrap();
    assert_eq!(
        clock.calls(),
        1,
        "ACPI PM load_state should sample Clock::now_ns once when re-anchoring the PM timer"
    );

    // Forward-compatible snapshots: restore via ticks-only fallback.
    clock.reset_calls();
    const TAG_PM_TIMER_TICKS: u16 = 8;
    let mut w = SnapshotWriter::new(*b"ACPM", SnapshotVersion::new(1, 0));
    w.field_u32(TAG_PM_TIMER_TICKS, 0x00AB_CDEF);
    pm.load_state(&w.finish()).unwrap();
    assert_eq!(
        clock.calls(),
        1,
        "ticks-only ACPI PM load_state should sample Clock::now_ns once"
    );
}
