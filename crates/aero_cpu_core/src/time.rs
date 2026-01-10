//! Virtual time source for the CPU core.
//!
//! Windows 7 userland and kernel code frequently uses `RDTSC`/`RDTSCP` for
//! profiling, spin-loop timeouts, and entropy. In a browser (or any sandboxed
//! host) we need a deterministic/virtualized timestamp counter instead of
//! relying on host cycle counters.

use std::time::{Duration, Instant};

/// Default virtual timestamp counter frequency (3 GHz).
pub const DEFAULT_TSC_HZ: u64 = 3_000_000_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimeSourceMode {
    /// TSC advances only when the emulator retires instructions/cycles.
    Deterministic,
    /// TSC is derived from host wall clock and scaled by `tsc_hz`.
    ///
    /// This mode is inherently non-deterministic and should only be used for
    /// "real time" integrations.
    WallClock { anchor: Instant, anchor_tsc: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimeSource {
    tsc_hz: u64,
    tsc: u64,
    mode: TimeSourceMode,
}

impl Default for TimeSource {
    fn default() -> Self {
        Self::new_deterministic(DEFAULT_TSC_HZ)
    }
}

impl TimeSource {
    pub fn new_deterministic(tsc_hz: u64) -> Self {
        Self {
            tsc_hz,
            tsc: 0,
            mode: TimeSourceMode::Deterministic,
        }
    }

    pub fn new_wallclock(tsc_hz: u64) -> Self {
        let now = Instant::now();
        Self {
            tsc_hz,
            tsc: 0,
            mode: TimeSourceMode::WallClock {
                anchor: now,
                anchor_tsc: 0,
            },
        }
    }

    pub fn tsc_hz(&self) -> u64 {
        self.tsc_hz
    }

    pub fn set_tsc_hz(&mut self, tsc_hz: u64) {
        let current = self.read_tsc();
        self.tsc_hz = tsc_hz;
        self.set_tsc(current);
    }

    pub fn set_tsc(&mut self, tsc: u64) {
        self.tsc = tsc;

        if let TimeSourceMode::WallClock {
            anchor,
            anchor_tsc,
        } = &mut self.mode
        {
            *anchor = Instant::now();
            *anchor_tsc = tsc;
        }
    }

    pub fn read_tsc(&mut self) -> u64 {
        match &mut self.mode {
            TimeSourceMode::Deterministic => self.tsc,
            TimeSourceMode::WallClock {
                anchor,
                anchor_tsc,
            } => {
                let elapsed = Instant::now().saturating_duration_since(*anchor);
                let ticks = duration_to_ticks(self.tsc_hz, elapsed);
                self.tsc = anchor_tsc.wrapping_add(ticks);
                self.tsc
            }
        }
    }

    pub fn advance_cycles(&mut self, cycles: u64) {
        if matches!(&self.mode, TimeSourceMode::Deterministic) {
            self.tsc = self.tsc.wrapping_add(cycles);
            return;
        }

        // Wall-clock mode needs to re-anchor after manual advancement to keep scaling intact.
        let current = self.read_tsc();
        self.set_tsc(current.wrapping_add(cycles));
    }
}

fn duration_to_ticks(tsc_hz: u64, duration: Duration) -> u64 {
    if tsc_hz == 0 {
        return 0;
    }

    let nanos = duration.as_nanos();
    let ticks = nanos.saturating_mul(tsc_hz as u128) / 1_000_000_000u128;
    ticks.min(u64::MAX as u128) as u64
}

