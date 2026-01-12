//! Audio time-to-frame conversion without cumulative rounding drift.
//!
//! Many parts of the emulator operate in units of *audio frames* while the host/guest scheduler
//! advances in nanoseconds. Converting `delta_ns` → `frames` via integer division in every tick
//! (e.g. `frames += delta_ns * sample_rate / 1e9`) causes rounding error to accumulate when `delta`
//! is small or jittery.
//!
//! `AudioFrameClock` avoids that drift by keeping an explicit remainder accumulator (`frac_fp`)
//! and carrying it across calls. The accumulator is stored in a fixed-point form where:
//!
//! * 1 second = 1_000_000_000 "fraction" units.
//! * `frac_fp` is the leftover numerator from `ns * sample_rate_hz` divided by 1e9.
//!
//! This representation is exact for integer sample rates and nanosecond deltas and ensures that
//! splitting a time interval into many small steps yields the same total number of frames as a
//! single large step.
/// Deterministic audio frame scheduler driven by a monotonic nanosecond clock.
#[derive(Debug, Clone)]
pub struct AudioFrameClock {
    /// Audio sample rate to generate/consume at (frames per second).
    pub sample_rate_hz: u32,
    /// Last time passed to [`advance_to`].
    pub last_time_ns: u64,
    /// Fractional remainder accumulator.
    ///
    /// This is the remainder from dividing `delta_ns * sample_rate_hz + frac_fp` by
    /// 1_000_000_000 (nanoseconds per second). It is therefore always `< 1_000_000_000`.
    pub frac_fp: u64,
}

impl AudioFrameClock {
    const NS_PER_SEC: u64 = 1_000_000_000;

    pub fn new(sample_rate_hz: u32, start_time_ns: u64) -> Self {
        // Treat the sample rate as untrusted (e.g. host config / snapshot restore). Clamp to a
        // reasonable range to avoid division-by-zero and accidental multi-billion frame deltas.
        let sample_rate_hz = sample_rate_hz.clamp(1, crate::MAX_HOST_SAMPLE_RATE_HZ);
        Self {
            sample_rate_hz,
            last_time_ns: start_time_ns,
            frac_fp: 0,
        }
    }

    /// Advance the clock to `now_ns` and return the number of audio frames that elapsed since the
    /// previous call.
    ///
    /// If `now_ns` is earlier than the last observed time, the delta is treated as 0 and the
    /// internal time does not move backwards.
    pub fn advance_to(&mut self, now_ns: u64) -> usize {
        if now_ns <= self.last_time_ns {
            return 0;
        }

        let delta_ns = now_ns - self.last_time_ns;
        self.last_time_ns = now_ns;

        let total =
            (self.frac_fp as u128) + (delta_ns as u128).saturating_mul(self.sample_rate_hz as u128);
        let frames = total / (Self::NS_PER_SEC as u128);
        self.frac_fp = (total % (Self::NS_PER_SEC as u128)) as u64;

        usize::try_from(frames).unwrap_or(usize::MAX)
    }
}
