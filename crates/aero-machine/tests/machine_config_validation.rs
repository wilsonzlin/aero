use aero_machine::{Machine, MachineConfig, MachineError};

#[test]
fn cpu_count_must_be_non_zero() {
    let cfg = MachineConfig {
        cpu_count: 0,
        ..Default::default()
    };

    assert!(matches!(
        Machine::new(cfg),
        Err(MachineError::InvalidCpuCount(0))
    ));
}

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
fn enable_nvme_requires_enable_pc_platform() {
    let cfg = MachineConfig {
        enable_pc_platform: false,
        enable_nvme: true,
        ..Default::default()
    };

    assert!(matches!(
        Machine::new(cfg),
        Err(MachineError::NvmeRequiresPcPlatform)
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
fn enable_virtio_blk_requires_enable_pc_platform() {
    let cfg = MachineConfig {
        enable_pc_platform: false,
        enable_virtio_blk: true,
        ..Default::default()
    };

    assert!(matches!(
        Machine::new(cfg),
        Err(MachineError::VirtioBlkRequiresPcPlatform)
    ));
}

#[test]
fn enable_uhci_requires_enable_pc_platform() {
    let cfg = MachineConfig {
        enable_pc_platform: false,
        enable_uhci: true,
        ..Default::default()
    };

    assert!(matches!(
        Machine::new(cfg),
        Err(MachineError::UhciRequiresPcPlatform)
    ));
}

#[test]
fn enable_aerogpu_requires_enable_pc_platform() {
    let cfg = MachineConfig {
        enable_pc_platform: false,
        enable_aerogpu: true,
        ..Default::default()
    };

    assert!(matches!(
        Machine::new(cfg),
        Err(MachineError::AeroGpuRequiresPcPlatform)
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

#[test]
fn browser_defaults_preset_is_valid_and_stable() {
    let cfg = MachineConfig::browser_defaults(2 * 1024 * 1024);

    assert_eq!(cfg.cpu_count, 1);
    assert!(cfg.enable_pc_platform);

    // Win7 storage topology.
    assert!(cfg.enable_ahci);
    assert!(cfg.enable_ide);
    assert!(!cfg.enable_nvme);
    assert!(!cfg.enable_virtio_blk);

    // Browser runtime devices.
    assert!(cfg.enable_e1000);
    assert!(!cfg.enable_virtio_net);
    assert!(cfg.enable_uhci);

    // Deterministic core devices.
    assert!(cfg.enable_vga);
    assert!(!cfg.enable_aerogpu);
    assert!(cfg.enable_serial);
    assert!(cfg.enable_i8042);
    assert!(cfg.enable_a20_gate);
    assert!(cfg.enable_reset_ctrl);

    Machine::new(cfg).expect("MachineConfig::browser_defaults should pass Machine::new validation");
}
