//! VBlank / scanout timing helpers for the AeroGPU device model.
//!
//! The device model exposes a guest-visible vblank period register expressed in nanoseconds. Hosts
//! typically configure it from a `vblank_hz` rate (e.g. 60Hz), so we provide small shared helpers to
//! keep the arithmetic consistent across wrappers (PCI device, bare BAR0 device, etc.).

/// Convert a vblank refresh rate (Hz) into a vblank period in nanoseconds.
///
/// Returns `None` when vblank is disabled (`None` or `Some(0)`).
pub fn period_ns_from_hz(vblank_hz: Option<u32>) -> Option<u64> {
    vblank_hz.and_then(|hz| {
        if hz == 0 {
            return None;
        }
        // Use ceil division to keep 60 Hz at 16_666_667 ns (rather than truncating to 16_666_666).
        Some(1_000_000_000u64.div_ceil(hz as u64))
    })
}

/// Clamp a vblank period (nanoseconds) to the guest-visible `u32` register representation.
pub fn period_ns_to_reg(period_ns: u64) -> u32 {
    period_ns.min(u64::from(u32::MAX)) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn period_ns_from_hz_uses_ceil_division() {
        assert_eq!(period_ns_from_hz(None), None);
        assert_eq!(period_ns_from_hz(Some(0)), None);
        assert_eq!(period_ns_from_hz(Some(1)), Some(1_000_000_000));
        assert_eq!(period_ns_from_hz(Some(60)), Some(16_666_667));
    }

    #[test]
    fn period_ns_to_reg_clamps_to_u32() {
        assert_eq!(period_ns_to_reg(0), 0);
        assert_eq!(period_ns_to_reg(u64::from(u32::MAX)), u32::MAX);
        assert_eq!(period_ns_to_reg(u64::from(u32::MAX) + 1), u32::MAX);
    }
}
