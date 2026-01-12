use aero_machine::{Machine, MachineConfig, MachineError};

#[test]
fn enable_e1000_requires_enable_pc_platform() {
    let cfg = MachineConfig {
        enable_pc_platform: false,
        enable_e1000: true,
        ..Default::default()
    };

    assert!(matches!(
        Machine::new(cfg),
        Err(MachineError::E1000RequiresPcPlatform)
    ));
}

