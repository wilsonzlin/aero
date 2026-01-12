use aero_devices::reset_ctrl::RESET_CTRL_RESET_VALUE;
use aero_pc_platform::{PcPlatform, ResetEvent};

#[test]
fn pc_platform_reset_restores_deterministic_power_on_state() {
    let mut pc = PcPlatform::new(2 * 1024 * 1024);

    // Mutate some state:
    // - Enable A20.
    pc.io.write_u8(0x92, 0x02);
    assert!(pc.chipset.a20().enabled());

    // - Touch the PCI config address latch (0xCF8).
    pc.io.write(0xCF8, 4, 0x8000_0000);
    assert_eq!(pc.io.read(0xCF8, 4), 0x8000_0000);

    // - Queue a reset event.
    pc.io.write_u8(0xCF9, RESET_CTRL_RESET_VALUE);
    assert_eq!(pc.take_reset_events(), vec![ResetEvent::System]);
    pc.io.write_u8(0xCF9, RESET_CTRL_RESET_VALUE);

    // Now reset back to baseline.
    pc.reset();

    // A20 must be disabled.
    assert!(!pc.chipset.a20().enabled());

    // Reset should clear any pending reset events.
    assert!(pc.take_reset_events().is_empty());

    // PCI config address latch should be cleared.
    assert_eq!(pc.io.read(0xCF8, 4), 0);
}

