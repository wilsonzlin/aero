use aero_machine::{MachineError, PcMachine, PcMachineConfig};

#[test]
fn pc_machine_cpu_count_must_be_non_zero() {
    let cfg = PcMachineConfig {
        cpu_count: 0,
        ..Default::default()
    };

    assert!(matches!(
        PcMachine::new_with_config(cfg),
        Err(MachineError::InvalidCpuCount(0))
    ));
}
