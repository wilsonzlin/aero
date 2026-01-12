use std::cell::{Cell, RefCell};
use std::rc::Rc;

use aero_devices::acpi_pm::{AcpiPmCallbacks, AcpiPmConfig, AcpiPmIo, PM1_STS_PWRBTN};
use aero_devices::clock::{Clock, ManualClock};
use aero_devices::irq::IrqLine;
use aero_io_snapshot::io::state::{
    IoSnapshot, SnapshotError, SnapshotReader, SnapshotVersion, SnapshotWriter,
};
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
fn snapshot_restore_without_elapsed_ns_falls_back_to_pm_timer_ticks() {
    const TAG_PM_TIMER_ELAPSED_NS: u16 = 6;

    let cfg = AcpiPmConfig::default();

    let clock0 = ManualClock::new();
    let mut pm0 =
        AcpiPmIo::new_with_callbacks_and_clock(cfg, AcpiPmCallbacks::default(), clock0.clone());

    clock0.set_ns(1_000_000);
    clock0.advance_ns(123_456_789);
    let t0 = pm0.read(cfg.pm_tmr_blk, 4);

    let snap_full = pm0.save_state();
    let r = SnapshotReader::parse(
        &snap_full,
        <AcpiPmIo<ManualClock> as IoSnapshot>::DEVICE_ID,
    )
    .unwrap();

    // Re-encode a snapshot that preserves all fields except elapsed-ns, forcing load_state()
    // to use the tick-count fallback path.
    let header = r.header();
    let mut w = SnapshotWriter::new(header.device_id, header.device_version);
    for tag in 1u16..=9 {
        if tag == TAG_PM_TIMER_ELAPSED_NS {
            continue;
        }
        if let Some(bytes) = r.bytes(tag) {
            w.field_bytes(tag, bytes.to_vec());
        }
    }
    let snap_no_elapsed = w.finish();

    let r2 = SnapshotReader::parse(
        &snap_no_elapsed,
        <AcpiPmIo<ManualClock> as IoSnapshot>::DEVICE_ID,
    )
    .unwrap();
    assert!(
        r2.u64(TAG_PM_TIMER_ELAPSED_NS).unwrap().is_none(),
        "test snapshot must omit TAG_PM_TIMER_ELAPSED_NS"
    );

    let clock1 = ManualClock::new();
    clock1.set_ns(9_000_000);
    let mut pm1 =
        AcpiPmIo::new_with_callbacks_and_clock(cfg, AcpiPmCallbacks::default(), clock1.clone());
    pm1.load_state(&snap_no_elapsed).unwrap();

    let t1 = pm1.read(cfg.pm_tmr_blk, 4);
    assert_eq!(t0, t1, "PM_TMR must match after restore from tick-only snapshot");
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

#[test]
fn snapshot_load_ignores_invalid_tick_field_when_elapsed_ns_is_present() {
    const TAG_PM_TIMER_ELAPSED_NS: u16 = 6;
    const TAG_PM_TIMER_TICKS: u16 = 8;

    let cfg = AcpiPmConfig::default();
    let clock = ManualClock::new();
    clock.set_ns(10_000_000_000);

    let mut pm =
        AcpiPmIo::new_with_callbacks_and_clock(cfg, AcpiPmCallbacks::default(), clock.clone());

    // Provide a valid elapsed-ns field and a deliberately corrupted tick field. `load_state` should
    // restore the PM timer from elapsed-ns and ignore the invalid redundant tick field.
    let mut w = SnapshotWriter::new(*b"ACPM", SnapshotVersion::new(1, 0));
    w.field_u64(TAG_PM_TIMER_ELAPSED_NS, 1_000_000_000);
    w.field_bytes(TAG_PM_TIMER_TICKS, vec![0xAA]); // invalid encoding for u32
    pm.load_state(&w.finish()).unwrap();

    assert_eq!(pm.read(cfg.pm_tmr_blk, 4) & 0x00FF_FFFF, 3_579_545);
}

#[test]
fn snapshot_restore_from_ticks_and_remainder_without_elapsed_ns_preserves_phase() {
    const TAG_PM_TIMER_TICKS: u16 = 8;
    const TAG_PM_TIMER_REMAINDER: u16 = 9;

    let cfg = AcpiPmConfig::default();

    // Create a timer state that is just shy of the first tick, so advancing by 1ns crosses the
    // tick boundary. This makes it easy to detect whether fractional remainder was restored.
    let mut pm = AcpiPmIo::new(cfg);
    pm.advance_ns(279); // 279ns < 1 tick, but 279ns * 3.579545MHz is close to 1e9.
    assert_eq!(pm.read(cfg.pm_tmr_blk, 4) & 0x00FF_FFFF, 0);

    let snap = pm.save_state();
    let r = SnapshotReader::parse(&snap, *b"ACPM").unwrap();
    let ticks = r.u32(TAG_PM_TIMER_TICKS).unwrap().unwrap();
    let remainder = r.u32(TAG_PM_TIMER_REMAINDER).unwrap().unwrap();

    // Build a snapshot that omits the preferred `elapsed_ns` field, forcing the restore path to
    // reconstruct the timer from `(ticks, remainder)`.
    let mut w = SnapshotWriter::new(*b"ACPM", SnapshotVersion::new(1, 0));
    w.field_u32(TAG_PM_TIMER_TICKS, ticks);
    w.field_u32(TAG_PM_TIMER_REMAINDER, remainder);
    let bytes = w.finish();

    let mut restored = AcpiPmIo::new(cfg);
    restored.load_state(&bytes).unwrap();
    assert_eq!(restored.read(cfg.pm_tmr_blk, 4) & 0x00FF_FFFF, 0);

    restored.advance_ns(1);
    assert_eq!(
        restored.read(cfg.pm_tmr_blk, 4) & 0x00FF_FFFF,
        1,
        "restored PM_TMR must preserve sub-tick remainder (1ns should cross the first tick boundary)"
    );
}

#[test]
fn snapshot_restore_from_ticks_and_remainder_with_wrap_restores_phase() {
    const PM_TIMER_HZ: u128 = 3_579_545;
    const NS_PER_SEC: u128 = 1_000_000_000;

    let cfg = AcpiPmConfig::default();
    let clock = ManualClock::new();
    clock.set_ns(9_000_000);

    let mut pm = AcpiPmIo::new_with_callbacks_and_clock(cfg, AcpiPmCallbacks::default(), clock);

    // Pick an elapsed time that exceeds 2^24 ticks so load_state must solve for the wrap count `k`
    // when reconstructing the timer from (ticks_mod, remainder).
    let elapsed_ns = 5_000_000_001u64;
    let numer = (elapsed_ns as u128) * PM_TIMER_HZ;
    let ticks_full = numer / NS_PER_SEC;
    let ticks_mod = (ticks_full as u32) & 0x00FF_FFFF;
    let remainder = (numer % NS_PER_SEC) as u32;

    const TAG_PM_TIMER_TICKS: u16 = 8;
    const TAG_PM_TIMER_REMAINDER: u16 = 9;
    let mut w = SnapshotWriter::new(*b"ACPM", SnapshotVersion::new(1, 0));
    w.field_u32(TAG_PM_TIMER_TICKS, ticks_mod);
    w.field_u32(TAG_PM_TIMER_REMAINDER, remainder);
    pm.load_state(&w.finish()).unwrap();

    assert_eq!(pm.read(cfg.pm_tmr_blk, 4) & 0x00FF_FFFF, ticks_mod);

    // Re-saving must preserve the remainder (phase). If load_state fell back to ticks-only
    // reconstruction, the remainder would almost certainly differ.
    let snap2 = pm.save_state();
    let r = SnapshotReader::parse(&snap2, *b"ACPM").unwrap();
    let remainder2 = r.u32(TAG_PM_TIMER_REMAINDER).unwrap().unwrap();
    assert_eq!(remainder2, remainder);
}

#[test]
fn snapshot_restore_from_ticks_and_remainder_with_wrap_preserves_tick_boundary() {
    // Similar to `snapshot_restore_from_ticks_and_remainder_with_wrap_restores_phase`, but this
    // test verifies observable phase by stepping across an expected tick boundary.
    const TAG_PM_TIMER_TICKS: u16 = 8;
    const TAG_PM_TIMER_REMAINDER: u16 = 9;
    const PM_TIMER_HZ: u128 = 3_579_545;
    const NS_PER_SEC: u128 = 1_000_000_000;
    const MASK_24BIT: u32 = 0x00FF_FFFF;

    let cfg = AcpiPmConfig::default();

    let mut pm = AcpiPmIo::new(cfg);
    // 10 seconds + 1ns => wrap count > 0 and a non-zero fractional remainder.
    pm.advance_ns(10_000_000_001);
    let t0 = pm.read(cfg.pm_tmr_blk, 4) & MASK_24BIT;

    let snap = pm.save_state();
    let r = SnapshotReader::parse(&snap, *b"ACPM").unwrap();
    let ticks = r.u32(TAG_PM_TIMER_TICKS).unwrap().unwrap();
    let remainder = r.u32(TAG_PM_TIMER_REMAINDER).unwrap().unwrap();

    // Omit the preferred `elapsed_ns` field, forcing restore from `(ticks, remainder)`.
    let mut w = SnapshotWriter::new(*b"ACPM", SnapshotVersion::new(1, 0));
    w.field_u32(TAG_PM_TIMER_TICKS, ticks);
    w.field_u32(TAG_PM_TIMER_REMAINDER, remainder);
    let bytes = w.finish();

    let mut restored = AcpiPmIo::new(cfg);
    restored.load_state(&bytes).unwrap();

    let t1 = restored.read(cfg.pm_tmr_blk, 4) & MASK_24BIT;
    assert_eq!(t1, t0);

    // The snapshot remainder is `(elapsed_ns * FREQ) % 1e9`; the next tick occurs once
    // `remainder + delta_ns * FREQ >= 1e9`.
    let remainder_u128 = u128::from(remainder);
    let ns_until_next_tick = (NS_PER_SEC - remainder_u128 + PM_TIMER_HZ - 1) / PM_TIMER_HZ;
    assert!(ns_until_next_tick >= 1);

    let before = (ns_until_next_tick - 1) as u64;
    pm.advance_ns(before);
    restored.advance_ns(before);
    assert_eq!(pm.read(cfg.pm_tmr_blk, 4) & MASK_24BIT, t0);
    assert_eq!(restored.read(cfg.pm_tmr_blk, 4) & MASK_24BIT, t0);

    pm.advance_ns(1);
    restored.advance_ns(1);
    let expected = (t0 + 1) & MASK_24BIT;
    assert_eq!(pm.read(cfg.pm_tmr_blk, 4) & MASK_24BIT, expected);
    assert_eq!(restored.read(cfg.pm_tmr_blk, 4) & MASK_24BIT, expected);
}

#[test]
fn snapshot_restore_with_inconsistent_remainder_falls_back_to_ticks_only() {
    // If `(pm_timer_ticks, pm_timer_remainder)` are inconsistent (no solution for the wrap count),
    // load_state should fall back to restoring the guest-visible tick counter only rather than
    // failing the entire restore.
    const TAG_PM_TIMER_TICKS: u16 = 8;
    const TAG_PM_TIMER_REMAINDER: u16 = 9;

    let cfg = AcpiPmConfig::default();
    let mut pm = AcpiPmIo::new(cfg);

    // Choose a remainder that is *not* divisible by 5. Since the PM timer frequency is divisible
    // by 5, valid snapshots always have `remainder % 5 == 0` (see the restore math in
    // `acpi_pm.rs`). This forces the solver to take the "no solution" fallback path.
    let ticks = 0x00AB_CDEFu32;
    let remainder = 1u32;

    let mut w = SnapshotWriter::new(*b"ACPM", SnapshotVersion::new(1, 0));
    w.field_u32(TAG_PM_TIMER_TICKS, ticks);
    w.field_u32(TAG_PM_TIMER_REMAINDER, remainder);
    pm.load_state(&w.finish()).unwrap();

    assert_eq!(
        pm.read(cfg.pm_tmr_blk, 4) & 0x00FF_FFFF,
        ticks & 0x00FF_FFFF,
        "fallback restore should preserve the visible 24-bit PM_TMR tick counter"
    );

    // Re-saving should produce a *valid* remainder (divisible by 5) rather than preserving the
    // corrupted remainder value from the snapshot.
    let snap2 = pm.save_state();
    let r = SnapshotReader::parse(&snap2, *b"ACPM").unwrap();
    let remainder2 = r.u32(TAG_PM_TIMER_REMAINDER).unwrap().unwrap();
    assert_ne!(remainder2, remainder);
    assert_eq!(remainder2 % 5, 0);
}

#[test]
fn snapshot_load_does_not_sample_clock_on_decode_error() {
    const TAG_PM_TIMER_TICKS: u16 = 8;

    let cfg = AcpiPmConfig::default();
    let clock = CountingClock::new(1);

    let mut pm =
        AcpiPmIo::new_with_callbacks_and_clock(cfg, AcpiPmCallbacks::default(), clock.clone());
    clock.reset_calls();

    // Mutate some state so we can confirm `load_state()` is atomic on failure.
    pm.write(cfg.pm1a_cnt_blk, 2, 0x1234);
    let pm1_cnt_before = pm.pm1_cnt();
    assert_eq!(clock.calls(), 0);

    // Provide a corrupted tick field and omit `elapsed_ns` to force decode of ticks.
    let mut w = SnapshotWriter::new(*b"ACPM", SnapshotVersion::new(1, 0));
    w.field_bytes(TAG_PM_TIMER_TICKS, vec![0xAA]); // invalid encoding for u32
    let bytes = w.finish();

    assert!(pm.load_state(&bytes).is_err());
    assert_eq!(
        clock.calls(),
        0,
        "ACPI PM load_state must not sample Clock::now_ns if snapshot decoding fails"
    );
    assert_eq!(
        pm.pm1_cnt(),
        pm1_cnt_before,
        "load_state must leave device state unchanged on decode error"
    );
}

#[test]
fn snapshot_save_samples_clock_once_for_timer_encoding() {
    // Snapshot save should sample the clock once for deterministic PM timer encoding. This guards
    // against regressions where `save_state()` would call `Clock::now_ns()` multiple times and
    // produce internally inconsistent timer fields under unusual clock implementations (e.g. test
    // clocks that advance on each `now_ns()` call).
    let cfg = AcpiPmConfig::default();
    let clock = CountingClock::new(1_000_000_000);

    let pm = AcpiPmIo::new_with_callbacks_and_clock(cfg, AcpiPmCallbacks::default(), clock.clone());
    clock.reset_calls();

    let _ = pm.save_state();
    assert_eq!(
        clock.calls(),
        1,
        "ACPI PM save_state should sample Clock::now_ns once when encoding PM timer fields"
    );
}

#[test]
fn snapshot_load_is_atomic_on_decode_error() {
    let cfg = AcpiPmConfig::default();
    let clock = ManualClock::new();
    clock.set_ns(1_000_000_000);
    let mut pm =
        AcpiPmIo::new_with_callbacks_and_clock(cfg, AcpiPmCallbacks::default(), clock.clone());

    // Move the PM timer away from zero so a partial restore that resets the timer base would be
    // detectable.
    clock.advance_ns(123_456_789);

    // Mutate some state so a partial restore that resets register state would be detectable.
    pm.write(cfg.pm1a_evt_blk + 2, 2, u32::from(PM1_STS_PWRBTN));
    pm.write(cfg.smi_cmd_port, 1, u32::from(cfg.acpi_enable_cmd));
    pm.trigger_power_button();
    pm.write(cfg.pm1a_cnt_blk, 2, 0x1235);
    let half = (cfg.gpe0_blk_len as u16) / 2;
    pm.write(cfg.gpe0_blk + half, 1, 0x02);

    let pm1_sts_before = pm.pm1_status();
    let pm1_cnt_before = pm.pm1_cnt();
    let gpe0_en_before = pm.read(cfg.gpe0_blk + half, 1);
    let sci_before = pm.sci_level();
    let tmr_before = pm.read(cfg.pm_tmr_blk, 4);

    // Construct a corrupted snapshot that includes a PM1_STS field with an invalid length for `u16`.
    const TAG_PM1_STS: u16 = 1;
    let mut w = SnapshotWriter::new(*b"ACPM", SnapshotVersion::new(1, 0));
    w.field_bytes(TAG_PM1_STS, vec![0xAA]); // should be 2 bytes
    let bytes = w.finish();

    assert!(pm.load_state(&bytes).is_err());

    assert_eq!(pm.pm1_status(), pm1_sts_before);
    assert_eq!(pm.pm1_cnt(), pm1_cnt_before);
    assert_eq!(pm.read(cfg.gpe0_blk + half, 1), gpe0_en_before);
    assert_eq!(pm.sci_level(), sci_before);
    assert_eq!(pm.read(cfg.pm_tmr_blk, 4), tmr_before);
}

#[test]
fn snapshot_restore_rejects_invalid_pm_timer_remainder() {
    const TAG_PM_TIMER_TICKS: u16 = 8;
    const TAG_PM_TIMER_REMAINDER: u16 = 9;

    let cfg = AcpiPmConfig::default();
    let mut pm = AcpiPmIo::new(cfg);

    let mut w = SnapshotWriter::new(*b"ACPM", SnapshotVersion::new(1, 0));
    w.field_u32(TAG_PM_TIMER_TICKS, 0);
    w.field_u32(TAG_PM_TIMER_REMAINDER, 1_000_000_000);
    let bytes = w.finish();

    assert!(
        matches!(
            pm.load_state(&bytes),
            Err(SnapshotError::InvalidFieldEncoding("pm_timer_remainder"))
        ),
        "invalid remainders must be rejected (expected SnapshotError::InvalidFieldEncoding)"
    );
}

#[test]
fn snapshot_restore_ignores_malformed_tick_fields_when_elapsed_ns_is_present() {
    const TAG_PM_TIMER_ELAPSED_NS: u16 = 6;
    const TAG_PM_TIMER_TICKS: u16 = 8;
    const TAG_PM_TIMER_REMAINDER: u16 = 9;

    let cfg = AcpiPmConfig::default();
    let mut pm = AcpiPmIo::new(cfg);

    // If elapsed-ns is present, load_state should not require tick-based fields to be valid; they
    // are redundant and should be ignored to maximize forward compatibility / robustness against
    // snapshot corruption in unused fields.
    let mut w = SnapshotWriter::new(*b"ACPM", SnapshotVersion::new(1, 0));
    w.field_u64(TAG_PM_TIMER_ELAPSED_NS, 0);
    w.field_bytes(TAG_PM_TIMER_TICKS, vec![0xAA]); // invalid u32 encoding
    w.field_bytes(TAG_PM_TIMER_REMAINDER, vec![0xBB]); // invalid u32 encoding
    let bytes = w.finish();

    pm.load_state(&bytes).unwrap();
    assert_eq!(
        pm.read(cfg.pm_tmr_blk, 4) & 0x00FF_FFFF,
        0,
        "restored PM_TMR should use elapsed-ns and start from 0"
    );
}
