use aero_machine::{Machine, MachineConfig};
use aero_platform::interrupts::PlatformInterruptMode;

#[test]
fn init_ipi_assert_resets_target_lapic_state() {
    let cfg = MachineConfig {
        cpu_count: 2,
        enable_pc_platform: true,
        // Keep the machine minimal; this test only needs LAPIC + IPI plumbing.
        enable_serial: false,
        enable_i8042: false,
        enable_vga: false,
        ..Default::default()
    };

    let mut m = Machine::new(cfg).unwrap();

    // Enable APIC mode so `get_pending_for_apic(1)` reports LAPIC1 vectors.
    let interrupts = m.platform_interrupts().expect("PC platform should be enabled");
    interrupts
        .borrow_mut()
        .set_mode(PlatformInterruptMode::Apic);

    // ---------------------------------------------------------------------
    // Mutate LAPIC1 state: change SVR + program a timer interrupt into IRR.
    // ---------------------------------------------------------------------
    // SVR: set software-enable with a non-default spurious vector (0xE0).
    m.write_lapic_u32(1, 0xF0, (1 << 8) | 0xE0);

    // Program LAPIC1 one-shot timer:
    // - Divide config = 0xB => divisor 1 (1ns per tick in our model).
    // - LVT timer = vector 0x55, unmasked.
    // - Initial count = 10 ticks.
    m.write_lapic_u32(1, 0x3E0, 0xB); // Divide config
    m.write_lapic_u32(1, 0x320, 0x55); // LVT Timer
    m.write_lapic_u32(1, 0x380, 10); // Initial count

    // Advance time enough to fire the timer and make the vector pending in IRR.
    m.tick_platform(10);

    assert_eq!(interrupts.borrow().get_pending_for_apic(1), Some(0x55));
    // IRR index for vector 0x55 is word 2 (vectors 0x40..=0x5F) at offset 0x220.
    let irr2 = m.read_lapic_u32(1, 0x220);
    assert_ne!(irr2 & (1 << (0x55 % 32)), 0, "expected IRR bit for vector 0x55 to be set");

    // ---------------------------------------------------------------------
    // Deliver INIT assert from BSP (APIC ID 0) to APIC ID 1.
    // ---------------------------------------------------------------------
    const ICR_LOW_OFF: u64 = 0x300;
    const ICR_HIGH_OFF: u64 = 0x310;
    let icr_high = 1u32 << 24;
    // INIT (0b101) + level=assert.
    let icr_init_low = (0b101u32 << 8) | (1u32 << 14);
    m.write_lapic_u32(0, ICR_HIGH_OFF, icr_high);
    m.write_lapic_u32(0, ICR_LOW_OFF, icr_init_low);

    // ---------------------------------------------------------------------
    // Assert LAPIC1 state is reset: SVR restored and pending vectors cleared.
    // ---------------------------------------------------------------------
    let svr = m.read_lapic_u32(1, 0xF0);
    assert_eq!(
        svr & 0x1FF,
        0x1FF,
        "expected SVR to be reset to platform baseline (software-enabled, spurious vector 0xFF)"
    );

    for i in 0..8u64 {
        let irr = m.read_lapic_u32(1, 0x200 + i * 0x10);
        assert_eq!(irr, 0, "expected IRR[{i}] to be cleared by INIT");
    }
    assert_eq!(
        interrupts.borrow().get_pending_for_apic(1),
        None,
        "expected no pending vectors after INIT"
    );
}

