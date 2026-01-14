use aero_cpu_core::state::RFLAGS_IF;
use aero_machine::{Machine, MachineConfig};

#[test]
fn smp_ap_starts_halted_waiting_for_sipi() {
    let cfg = MachineConfig {
        cpu_count: 2,
        enable_pc_platform: true,
        enable_serial: false,
        enable_vga: false,
        enable_i8042: false,
        ..Default::default()
    };

    let m = Machine::new(cfg).unwrap();

    let ap_state = m.vcpu_state(1).unwrap();
    assert!(
        ap_state.halted,
        "AP should start in a halted wait-for-SIPI state"
    );
    // Ensure maskable interrupts are disabled in the baseline RFLAGS so a `HLT`-like wait state
    // cannot be exited by a queued external interrupt until AP bring-up code runs.
    assert_eq!(ap_state.rflags() & RFLAGS_IF, 0);
}
