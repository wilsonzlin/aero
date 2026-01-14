use aero_cpu_core::state::RFLAGS_IF;
use aero_machine::{Machine, MachineConfig};
use aero_platform::interrupts::PlatformInterruptMode;

const IA32_APIC_BASE_BSP_BIT: u64 = 1 << 8;

#[test]
fn init_ipi_resets_target_ap_lapic_and_enters_wait_for_sipi() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        cpu_count: 2,
        enable_pc_platform: true,
        ..Default::default()
    })
    .unwrap();
    m.platform_interrupts()
        .expect("pc platform enabled")
        .borrow_mut()
        .set_mode(PlatformInterruptMode::Apic);

    // Dirty the AP's LAPIC state so we can verify INIT resets it.
    const LAPIC_SVR_OFF: u64 = 0xF0;
    // Non-default spurious vector (0xE0) + software-enable.
    let dirty_svr = (1u32 << 8) | 0xE0;
    m.write_lapic_u32(1, LAPIC_SVR_OFF, dirty_svr);
    assert_eq!(m.read_lapic_u32(1, LAPIC_SVR_OFF), dirty_svr);

    // Send an INIT IPI from the BSP to APIC ID 1 (vCPU1).
    const ICR_LOW_OFF: u64 = 0x300;
    const ICR_HIGH_OFF: u64 = 0x310;

    let icr_high = 1u32 << 24;
    // Delivery mode INIT (0b101) + Level=Assert.
    let icr_low = (0b101u32 << 8) | (1u32 << 14);
    m.write_lapic_u32(0, ICR_HIGH_OFF, icr_high);
    m.write_lapic_u32(0, ICR_LOW_OFF, icr_low);

    // LAPIC SVR should reset its vector to the power-on baseline (0xFF). The platform integration
    // keeps the LAPIC software-enable bit set so IOAPIC/MSI delivery continues to work after INIT
    // modelling.
    let svr = m.read_lapic_u32(1, LAPIC_SVR_OFF);
    assert_eq!(svr & 0x1FF, 0x1FF);
    assert_ne!(svr, dirty_svr);

    // vCPU architectural state should be reset to a real-mode baseline and halted (wait-for-SIPI).
    let ap = m.vcpu_state(1).unwrap();
    assert_eq!(ap.segments.cs.base, 0);
    assert_eq!(ap.get_ip(), 0);
    assert!(ap.halted);
    assert_eq!(ap.rflags() & RFLAGS_IF, 0);
    assert_eq!(ap.msr.apic_base & IA32_APIC_BASE_BSP_BIT, 0);
}
