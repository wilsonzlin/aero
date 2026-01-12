use aero_devices::acpi_pm::AcpiPmCallbacks;
use aero_devices::clock::Clock;
use aero_devices::acpi_pm::{AcpiPmConfig, AcpiPmIo};
use aero_platform::io::PortIoDevice;
use std::cell::Cell;
use std::rc::Rc;

#[test]
fn pm_tmr_advances_deterministically_via_advance_ns() {
    let cfg = AcpiPmConfig::default();

    // Deterministic reset behaviour: the PM timer starts at 0 and advances only
    // when explicitly ticked.
    let mut pm = AcpiPmIo::new(cfg);
    assert_eq!(pm.read(cfg.pm_tmr_blk, 4), 0);

    pm.advance_ns(1_000_000_000);
    assert_eq!(pm.read(cfg.pm_tmr_blk, 4), 3_579_545);

    // Advancing in different chunk sizes must yield the same observable counter.
    let mut pm_chunked = AcpiPmIo::new(cfg);
    pm_chunked.advance_ns(500_000_000);
    pm_chunked.advance_ns(500_000_000);
    assert_eq!(pm_chunked.read(cfg.pm_tmr_blk, 4), 3_579_545);

    // Ensure fractional tick remainder is carried across calls.
    let mut pm_remainder = AcpiPmIo::new(cfg);
    pm_remainder.advance_ns(1_000);
    assert_eq!(pm_remainder.read(cfg.pm_tmr_blk, 4), 3);
    pm_remainder.advance_ns(1_000);
    assert_eq!(pm_remainder.read(cfg.pm_tmr_blk, 4), 7);

    // PM_TMR is 24-bit; reads must wrap accordingly.
    let mut pm_wrap = AcpiPmIo::new(cfg);
    pm_wrap.advance_ns(5_000_000_000);
    let expected = (3_579_545u32 * 5) & 0x00FF_FFFF;
    assert_eq!(pm_wrap.read(cfg.pm_tmr_blk, 4), expected);

    // Reset should also reset the deterministic timebase.
    pm_wrap.reset();
    assert_eq!(pm_wrap.read(cfg.pm_tmr_blk, 4), 0);
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
fn pm_tmr_4byte_read_uses_single_clock_sample() {
    let cfg = AcpiPmConfig::default();
    let clock = CountingClock::new(1_000_000_000);

    let mut pm = AcpiPmIo::new_with_callbacks_and_clock(cfg, AcpiPmCallbacks::default(), clock.clone());
    clock.reset_calls();

    let _ = pm.read(cfg.pm_tmr_blk, 4);
    assert_eq!(
        clock.calls(),
        1,
        "PM_TMR multi-byte reads must not call Clock::now_ns once per byte"
    );
}
