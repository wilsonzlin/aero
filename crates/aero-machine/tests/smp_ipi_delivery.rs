use aero_machine::{Machine, MachineConfig};
use aero_platform::interrupts::PlatformInterruptMode;

/// Validate that INIT + SIPI updates the target AP's CPU state without executing guest code.
#[test]
fn smp_init_and_sipi_update_ap_cpu_state() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        cpu_count: 2,
        enable_pc_platform: true,
        // Keep the machine minimal; this test only needs the interrupt controller complex.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    };

    let mut m = Machine::new(cfg).unwrap();

    // Ensure APIC mode so LAPIC0 is the active interrupt controller and vCPU LAPIC state is live.
    m.platform_interrupts()
        .unwrap()
        .borrow_mut()
        .set_mode(PlatformInterruptMode::Apic);

    // Send an INIT IPI from the BSP to APIC ID 1 (vCPU1).
    const ICR_LOW_OFF: u64 = 0x300;
    const ICR_HIGH_OFF: u64 = 0x310;

    m.write_lapic_u32(0, ICR_HIGH_OFF, 1u32 << 24);
    // Delivery mode INIT (0b101) + Level=Assert.
    m.write_lapic_u32(0, ICR_LOW_OFF, (0b101u32 << 8) | (1u32 << 14));

    // After INIT, our integration leaves the AP halted waiting for a SIPI.
    assert!(m.cpu_by_index(1).halted);

    // Send a SIPI (STARTUP) to vector 0x08 (physical start address 0x8000).
    let sipi_vector = 0x08u32;
    m.write_lapic_u32(
        0,
        ICR_LOW_OFF,
        sipi_vector | (0b110u32 << 8) | (1u32 << 14),
    );

    let ap = m.cpu_by_index(1);
    assert!(!ap.halted);
    assert_eq!(ap.segments.cs.selector, 0x0800);
    assert_eq!(ap.segments.cs.base, 0x8000);
    assert_eq!(ap.rip(), 0);
}

/// Validate that fixed IPIs inject into the destination LAPIC without executing guest code.
#[test]
fn smp_fixed_ipi_sets_pending_vector_in_destination_lapic() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        cpu_count: 2,
        enable_pc_platform: true,
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    };

    let mut m = Machine::new(cfg).unwrap();

    let interrupts = m.platform_interrupts().unwrap();
    interrupts
        .borrow_mut()
        .set_mode(PlatformInterruptMode::Apic);

    const ICR_LOW_OFF: u64 = 0x300;
    const ICR_HIGH_OFF: u64 = 0x310;

    let vector = 0x45u8;

    // Destination APIC ID 1.
    m.write_lapic_u32(0, ICR_HIGH_OFF, 1u32 << 24);
    // Fixed delivery mode (0b000) + Level=Assert, vector in bits 0..7.
    m.write_lapic_u32(
        0,
        ICR_LOW_OFF,
        u32::from(vector) | (0b000u32 << 8) | (1u32 << 14),
    );

    assert_eq!(interrupts.borrow().get_pending_for_apic(1), Some(vector));
}

