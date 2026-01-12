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

#[test]
fn enable_ahci_requires_enable_pc_platform() {
    let cfg = MachineConfig {
        enable_pc_platform: false,
        enable_ahci: true,
        ..Default::default()
    };

    assert!(matches!(
        Machine::new(cfg),
        Err(MachineError::AhciRequiresPcPlatform)
    ));
}

#[test]
fn enable_ide_requires_enable_pc_platform() {
    let cfg = MachineConfig {
        enable_pc_platform: false,
        enable_ide: true,
        ..Default::default()
    };

    assert!(matches!(
        Machine::new(cfg),
        Err(MachineError::IdeRequiresPcPlatform)
    ));
}

#[test]
fn enable_virtio_net_requires_enable_pc_platform() {
    let cfg = MachineConfig {
        enable_pc_platform: false,
        enable_virtio_net: true,
        ..Default::default()
    };

    assert!(matches!(
        Machine::new(cfg),
        Err(MachineError::VirtioNetRequiresPcPlatform)
    ));
}

#[test]
fn enable_e1000_and_enable_virtio_net_are_mutually_exclusive() {
    let cfg = MachineConfig {
        enable_pc_platform: true,
        enable_e1000: true,
        enable_virtio_net: true,
        ..Default::default()
    };

    assert!(matches!(
        Machine::new(cfg),
        Err(MachineError::MultipleNicsEnabled)
    ));
}
