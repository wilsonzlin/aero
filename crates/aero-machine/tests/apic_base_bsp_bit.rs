use aero_machine::{Machine, MachineConfig};

const IA32_APIC_BASE_BSP_BIT: u64 = 1 << 8;

#[test]
fn ia32_apic_base_bsp_bit_is_set_only_on_vcpu0() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        cpu_count: 2,
        enable_pc_platform: true,
        ..Default::default()
    })
    .unwrap();

    assert_ne!(m.cpu().msr.apic_base & IA32_APIC_BASE_BSP_BIT, 0);
    assert_eq!(
        m.vcpu_state(1).unwrap().msr.apic_base & IA32_APIC_BASE_BSP_BIT,
        0
    );

    // Send an INIT IPI from the BSP to APIC ID 1 (vCPU1).
    const ICR_LOW_OFF: u64 = 0x300;
    const ICR_HIGH_OFF: u64 = 0x310;

    let icr_high = 1u32 << 24;
    // Delivery mode INIT (0b101) + Level=Assert.
    let icr_low = (0b101u32 << 8) | (1u32 << 14);
    m.write_lapic_u32(0, ICR_HIGH_OFF, icr_high);
    m.write_lapic_u32(0, ICR_LOW_OFF, icr_low);

    assert_eq!(
        m.vcpu_state(1).unwrap().msr.apic_base & IA32_APIC_BASE_BSP_BIT,
        0
    );
}
