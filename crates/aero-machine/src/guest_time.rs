//! Deterministic guest time accounting (instructions/cycles → nanoseconds).
//!
//! `aero_cpu_core` advances its deterministic `TSC` by a fixed number of cycles per retired
//! instruction (Tier-0 currently uses `1 cycle == 1 instruction`). Higher-level integrations that
//! drive platform devices/timers (PIT/HPET/LAPIC) need to convert that progress into an elapsed
//! duration (`delta_ns`).
//!
//! A naive integer conversion (`delta_ns = cycles * 1e9 / hz`) can produce `0` for small batches
//! when `hz > 1e9` (e.g. the default 3GHz TSC). Calling `PcPlatform::tick(0)` repeatedly stalls all
//! nanosecond-based timers.
//!
//! `GuestTime` avoids that stall (and cumulative rounding drift) by maintaining an explicit
//! remainder accumulator so fractional nanoseconds eventually "carry" into a non-zero `delta_ns`.

/// Default virtual CPU frequency used for guest time accounting.
///
/// This matches the default deterministic TSC frequency used by `aero_cpu_core`.
pub const DEFAULT_GUEST_CPU_HZ: u64 = aero_cpu_core::time::DEFAULT_TSC_HZ;

/// Deterministic instruction/cycle → nanosecond converter with remainder accumulation.
#[derive(Debug, Clone)]
pub struct GuestTime {
    /// Virtual CPU frequency used for converting cycles to nanoseconds.
    cpu_hz: u64,
    /// Remainder accumulator carried across calls.
    ///
    /// This is the remainder from dividing:
    /// `executed_cycles * 1_000_000_000 + remainder` by `cpu_hz`.
    remainder: u128,
}

impl GuestTime {
    const NS_PER_SEC: u128 = 1_000_000_000;

    /// Create a new deterministic guest time accumulator.
    ///
    /// `cpu_hz` should typically match the guest-visible TSC frequency (e.g.
    /// [`aero_cpu_core::time::DEFAULT_TSC_HZ`]).
    pub fn new(cpu_hz: u64) -> Self {
        Self {
            cpu_hz,
            remainder: 0,
        }
    }

    /// Construct a guest time accumulator that matches the current `CpuCore` TSC frequency.
    pub fn new_from_cpu(cpu: &aero_cpu_core::CpuCore) -> Self {
        Self::new(cpu.time.tsc_hz())
    }

    /// Returns the configured virtual CPU frequency in Hz.
    pub fn cpu_hz(&self) -> u64 {
        self.cpu_hz
    }

    /// Reset the accumulator (e.g. on VM reset).
    pub fn reset(&mut self) {
        self.remainder = 0;
    }

    /// Convert `executed` guest instructions/cycles into nanoseconds, accumulating remainder.
    ///
    /// Returns `delta_ns` suitable for passing to `PcPlatform::tick(delta_ns)`.
    pub fn advance_guest_time_for_instructions(&mut self, executed: u64) -> u64 {
        if executed == 0 || self.cpu_hz == 0 {
            return 0;
        }

        let numer = (executed as u128) * Self::NS_PER_SEC + self.remainder;
        let denom = self.cpu_hz as u128;

        let delta_ns = numer / denom;
        self.remainder = numer % denom;

        if delta_ns > u64::MAX as u128 {
            // Saturating behaviour is deterministic and avoids panics, but note that timekeeping
            // beyond this point is not meaningful for callers anyway.
            self.remainder = 0;
            u64::MAX
        } else {
            delta_ns as u64
        }
    }
}

impl Default for GuestTime {
    fn default() -> Self {
        Self::new(DEFAULT_GUEST_CPU_HZ)
    }
}
