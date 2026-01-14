use aero_cpu_core::state::RFLAGS_IF;
use aero_machine::{Machine, MachineConfig};

const IA32_APIC_BASE_BSP_BIT: u64 = 1 << 8;

#[test]
fn init_ipi_resets_target_ap_lapic_and_enters_wait_for_sipi() {
    let mut cfg = MachineConfig::default();
    cfg.ram_size_bytes = 2 * 1024 * 1024;
    cfg.cpu_count = 2;
    cfg.enable_pc_platform = true;

    let mut m = Machine::new(cfg).unwrap();

    // Dirty the AP's LAPIC state so we can verify INIT resets it.
    const LAPIC_SVR_OFF: u64 = 0xF0;
    let dirty_svr = (1u32 << 8) | 0xFF;
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

    // LAPIC SVR should reset to power-on default (0xFF with software-enable bit clear).
    let svr = m.read_lapic_u32(1, LAPIC_SVR_OFF);
    assert_eq!(svr, 0xFF);
    assert_eq!(svr & (1 << 8), 0);

    // vCPU architectural state should be reset to a real-mode baseline and halted (wait-for-SIPI).
    let ap = m.vcpu_state(1).unwrap();
    assert_eq!(ap.segments.cs.base, 0);
    assert_eq!(ap.get_ip(), 0);
    assert!(ap.halted);
    assert_eq!(ap.rflags() & RFLAGS_IF, 0);
    assert_eq!(ap.msr.apic_base & IA32_APIC_BASE_BSP_BIT, 0);
}

