use aero_machine::{Machine, MachineConfig};
use pretty_assertions::assert_eq;

#[test]
fn smp_vcpu_msr_state_has_correct_bsp_bit_and_optional_tsc_aux() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        cpu_count: 2,
        // Keep the machine minimal; the test only inspects vCPU reset state.
        enable_pc_platform: false,
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    };

    let m = Machine::new(cfg).expect("machine should construct with cpu_count=2");
    assert_eq!(m.cpu_count(), 2);

    let vcpu0 = m.vcpu_state(0).expect("vcpu0 state");
    let vcpu1 = m.vcpu_state(1).expect("vcpu1 state");

    // IA32_APIC_BASE[8] is the BSP flag.
    let bsp_bit = 1u64 << 8;
    assert_eq!(vcpu0.msr.apic_base & bsp_bit, bsp_bit, "vcpu0 must be BSP");
    assert_eq!(
        vcpu1.msr.apic_base & bsp_bit,
        0,
        "vcpu1 must not have the BSP bit set"
    );

    // Optional: if the SMP implementation sets IA32_TSC_AUX to the APIC ID, validate it.
    //
    // If unset, both vCPUs should remain at the default reset value (0).
    if vcpu0.msr.tsc_aux != 0 || vcpu1.msr.tsc_aux != 0 {
        assert_eq!(vcpu0.msr.tsc_aux, 0);
        assert_eq!(vcpu1.msr.tsc_aux, 1);
    }
}
