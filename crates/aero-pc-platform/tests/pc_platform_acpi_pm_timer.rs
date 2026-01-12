use aero_devices::acpi_pm::DEFAULT_PM_TMR_BLK;
use aero_pc_platform::{PcPlatform, PcPlatformConfig};

#[test]
fn pc_platform_tick_advances_acpi_pm_timer() {
    // Disable UHCI in this test so `PcPlatform::tick` stays O(1) (no per-millisecond loop).
    let mut pc = PcPlatform::new_with_config(
        2 * 1024 * 1024,
        PcPlatformConfig {
            enable_uhci: false,
            enable_ahci: false,
            ..Default::default()
        },
    );

    const MASK_24BIT: u32 = 0x00FF_FFFF;
    const PM_TIMER_HZ: u128 = 3_579_545;
    const NS_PER_SEC: u128 = 1_000_000_000;

    let before = pc.io.read(DEFAULT_PM_TMR_BLK, 4) & MASK_24BIT;
    assert_eq!(before, 0, "PM_TMR should start from a deterministic 0 value");

    // Pick a large delta so we'd clearly diverge if PM_TMR were accidentally driven by wall-clock
    // time rather than by `PcPlatform::tick`.
    let delta_ns: u64 = 5_000_000_000;
    pc.tick(delta_ns);

    let after = pc.io.read(DEFAULT_PM_TMR_BLK, 4) & MASK_24BIT;

    let expected_ticks = (((delta_ns as u128) * PM_TIMER_HZ) / NS_PER_SEC) as u32 & MASK_24BIT;
    let advanced_ticks = after.wrapping_sub(before) & MASK_24BIT;
    assert_eq!(advanced_ticks, expected_ticks);
}

