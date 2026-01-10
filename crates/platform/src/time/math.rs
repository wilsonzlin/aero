/// Number of nanoseconds in one second.
pub const NANOS_PER_SEC: u64 = 1_000_000_000;

/// Greatest common divisor for `u64`.
pub const fn gcd_u64(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        let r = a % b;
        a = b;
        b = r;
    }
    a
}

/// Reduces `num/denom` by dividing through by their GCD.
///
/// # Panics
///
/// Panics if `denom == 0`.
pub const fn reduce_fraction(num: u64, denom: u64) -> (u64, u64) {
    assert!(denom != 0, "denominator must be non-zero");
    let g = gcd_u64(num, denom);
    (num / g, denom / g)
}

/// Computes `floor(value * mul / div)` using `u128` to avoid overflow.
///
/// # Panics
///
/// Panics if `div == 0` or if the result does not fit in `u64`.
pub fn mul_div_u64_floor(value: u64, mul: u64, div: u64) -> u64 {
    assert!(div != 0, "division by zero");
    let result = (value as u128) * (mul as u128) / (div as u128);
    result
        .try_into()
        .expect("mul_div_u64_floor result overflowed u64::MAX")
}

/// Converts a number of ticks at `tick_freq_hz` into nanoseconds, using
/// `floor(ticks * 1e9 / tick_freq_hz)`.
pub fn ns_from_ticks_floor(ticks: u64, tick_freq_hz: u64) -> u64 {
    mul_div_u64_floor(ticks, NANOS_PER_SEC, tick_freq_hz)
}

/// Converts a duration in nanoseconds into ticks at `tick_freq_hz`, using
/// `floor(ns * tick_freq_hz / 1e9)`.
pub fn ticks_from_ns_floor(ns: u64, tick_freq_hz: u64) -> u64 {
    mul_div_u64_floor(ns, tick_freq_hz, NANOS_PER_SEC)
}

/// Returns a reduced rational representation of the period in nanoseconds for a
/// clock running at `freq_hz`.
///
/// The returned fraction is `period_num_ns / period_denom` such that:
///
/// `period = 1e9 / freq_hz` seconds, expressed in nanoseconds.
pub const fn period_from_hz_ns(freq_hz: u64) -> (u64, u64) {
    assert!(freq_hz != 0, "frequency must be non-zero");
    reduce_fraction(NANOS_PER_SEC, freq_hz)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gcd_and_reduce_work() {
        assert_eq!(gcd_u64(0, 5), 5);
        assert_eq!(gcd_u64(54, 24), 6);
        assert_eq!(reduce_fraction(10, 5), (2, 1));
        assert_eq!(reduce_fraction(1_000_000_000, 3_579_545), (200_000_000, 715_909));
    }

    #[test]
    fn ticks_and_ns_conversions_use_floor() {
        // 2 Hz => 0.5s per tick => 500ms.
        assert_eq!(ns_from_ticks_floor(1, 2), 500_000_000);
        assert_eq!(ticks_from_ns_floor(500_000_000, 2), 1);
        assert_eq!(ticks_from_ns_floor(499_999_999, 2), 0);
    }

    #[test]
    fn period_from_hz_is_reduced() {
        assert_eq!(period_from_hz_ns(2), (500_000_000, 1));
        assert_eq!(period_from_hz_ns(1_000_000_000), (1, 1));
    }
}

