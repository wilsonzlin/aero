use aero_machine::{MachineError, PcMachine, PcMachineConfig};

#[test]
fn pc_machine_cpu_count_must_be_non_zero() {
    let cfg = PcMachineConfig {
        cpu_count: 0,
        ..Default::default()
    };

    let err = PcMachine::new_with_config(cfg)
        .err()
        .expect("PcMachine::new_with_config with cpu_count==0 should fail");
    assert!(matches!(err, MachineError::InvalidCpuCount(0)));

    let msg = err.to_string();
    assert!(
        msg.contains("must be >= 1"),
        "message should be actionable: {msg}"
    );
}
