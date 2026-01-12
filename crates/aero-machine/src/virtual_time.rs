//! Helpers for deterministic virtual-time advancement in the machine integration layer.
//!
//! The CPU core advances its own virtual timestamp counter (TSC) in units of "retired cycles"
//! (currently: retired instructions). Many platform models, however, are driven by nanosecond
//! deltas.
//!
//! This module provides a small integer scaler for converting cycles to nanoseconds in a
//! deterministic way, including progress across multiple small slices that individually execute
//! too few cycles to produce a whole nanosecond.

/// Deterministically convert a stream of "retired cycles" to whole nanoseconds.
///
/// This uses integer arithmetic and keeps a remainder accumulator (in the numerator space) so
/// callers can feed arbitrarily small cycle deltas and still observe time progressing once enough
/// cycles have accumulated.
///
/// The conversion is:
///
/// ```text
/// total      = remainder + cycles * 1_000_000_000
/// delta_ns   = total / tsc_hz
/// remainder  = total % tsc_hz
/// ```
///
/// where `tsc_hz` is in cycles per second.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CycleToNs {
    /// Virtual TSC frequency in Hz (cycles per second).
    pub tsc_hz: u64,
    remainder: u128,
}

impl CycleToNs {
    const NANOS_PER_SEC: u128 = 1_000_000_000u128;

    /// Create a new scaler with the given TSC frequency.
    pub fn new(tsc_hz: u64) -> Self {
        Self {
            tsc_hz,
            remainder: 0,
        }
    }

    /// Convert `cycles` into whole nanoseconds and advance the internal remainder accumulator.
    ///
    /// If `tsc_hz == 0`, this returns `0` and clears any existing remainder.
    pub fn advance_cycles(&mut self, cycles: u64) -> u64 {
        if self.tsc_hz == 0 {
            self.remainder = 0;
            return 0;
        }

        let total = self.remainder + (cycles as u128) * Self::NANOS_PER_SEC;
        let hz = self.tsc_hz as u128;
        let delta_ns = (total / hz) as u64;
        self.remainder = total % hz;
        delta_ns
    }
}

#[cfg(test)]
mod tests {
    use super::CycleToNs;
    use aero_cpu_core::time::DEFAULT_TSC_HZ;
    use pretty_assertions::assert_eq;

    #[test]
    fn small_cycle_behavior_default_tsc_hz() {
        let mut scaler = CycleToNs::new(DEFAULT_TSC_HZ);
        assert_eq!(scaler.advance_cycles(1), 0);

        let mut scaler = CycleToNs::new(DEFAULT_TSC_HZ);
        assert_eq!(scaler.advance_cycles(3), 1);
    }

    #[test]
    fn remainder_carries_across_multiple_calls() {
        let mut scaler = CycleToNs::new(DEFAULT_TSC_HZ);

        // 1 cycle @ 3 GHz is 1/3 ns: should not advance time yet.
        assert_eq!(scaler.advance_cycles(1), 0);
        // Still not enough to reach 1 ns.
        assert_eq!(scaler.advance_cycles(1), 0);
        // Now we've accumulated 3 cycles total => 1 ns.
        assert_eq!(scaler.advance_cycles(1), 1);

        // Remainder should have returned to 0, so 3 more cycles is another 1 ns.
        assert_eq!(scaler.advance_cycles(3), 1);
    }

    #[test]
    fn large_cycle_behavior_is_stable_and_does_not_overflow() {
        let mut scaler = CycleToNs::new(DEFAULT_TSC_HZ);

        // u64::MAX is divisible by 3, so this should convert exactly without leaving a remainder.
        assert_eq!(scaler.advance_cycles(u64::MAX), u64::MAX / 3);

        // Now exercise the same path but with a remainder that must carry into the next call.
        let mut scaler = CycleToNs::new(DEFAULT_TSC_HZ);
        assert_eq!(scaler.advance_cycles(u64::MAX - 1), (u64::MAX - 1) / 3);
        // (u64::MAX - 1) % 3 == 2, so adding one cycle should complete another nanosecond.
        assert_eq!(scaler.advance_cycles(1), 1);
    }

    #[test]
    fn tsc_hz_zero_returns_zero_and_clears_remainder() {
        let mut scaler = CycleToNs::new(DEFAULT_TSC_HZ);
        assert_eq!(scaler.advance_cycles(1), 0);

        scaler.tsc_hz = 0;
        assert_eq!(scaler.advance_cycles(1), 0);

        // If remainder was not cleared, this would advance immediately.
        scaler.tsc_hz = DEFAULT_TSC_HZ;
        assert_eq!(scaler.advance_cycles(2), 0);
        assert_eq!(scaler.advance_cycles(1), 1);
    }
}

