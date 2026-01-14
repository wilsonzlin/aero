use aero_machine::{Machine, MachineConfig};

#[test]
fn lapic_mmio_is_routed_per_vcpu() {
    let cfg = MachineConfig {
        cpu_count: 2,
        enable_pc_platform: true,
        // Keep the machine minimal; this test only needs the interrupt controller complex.
        enable_serial: false,
        enable_i8042: false,
        enable_vga: false,
        ..Default::default()
    };

    let mut m = Machine::new(cfg).unwrap();

    // LAPIC ID register (REG_ID) is at offset 0x20; the APIC ID is in bits 24..31.
    let id0 = m.read_lapic_u32(0, 0x20) >> 24;
    let id1 = m.read_lapic_u32(1, 0x20) >> 24;
    assert_eq!(id0, 0);
    assert_eq!(id1, 1);
}

#[test]
fn lapic_mmio_cpu_ids_persist_after_machine_reset() {
    let cfg = MachineConfig {
        cpu_count: 4,
        enable_pc_platform: true,
        enable_serial: false,
        enable_i8042: false,
        enable_vga: false,
        ..Default::default()
    };

    let mut m = Machine::new(cfg).unwrap();

    for cpu in 0..4 {
        let id = m.read_lapic_u32(cpu, 0x20) >> 24;
        assert_eq!(id, cpu as u32);
    }

    // Ensure that `Machine::reset()` does not accidentally collapse a multi-LAPIC topology back to a
    // single BSP-only interrupt complex.
    m.reset();

    for cpu in 0..4 {
        let id = m.read_lapic_u32(cpu, 0x20) >> 24;
        assert_eq!(id, cpu as u32);
    }
}
