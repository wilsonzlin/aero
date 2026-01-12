use aero_devices::acpi_pm::{AcpiPmConfig, AcpiPmIo};
use aero_platform::io::PortIoDevice;

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

