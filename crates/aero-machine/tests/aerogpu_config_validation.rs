use aero_machine::{Machine, MachineConfig, MachineError};

#[test]
fn enable_aerogpu_requires_enable_pc_platform() {
    let cfg = MachineConfig {
        enable_pc_platform: false,
        enable_vga: false,
        enable_aerogpu: true,
        ..Default::default()
    };

    assert!(matches!(
        Machine::new(cfg),
        Err(MachineError::AeroGpuRequiresPcPlatform)
    ));
}

#[test]
fn enable_aerogpu_conflicts_with_enable_vga() {
    let cfg = MachineConfig {
        enable_pc_platform: true,
        enable_vga: true,
        enable_aerogpu: true,
        ..Default::default()
    };

    assert!(matches!(
        Machine::new(cfg),
        Err(MachineError::AeroGpuConflictsWithVga)
    ));
}
