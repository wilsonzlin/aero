use aero_machine::{Machine, MachineConfig, MachineError};

#[test]
fn cpu_count_must_be_non_zero() {
    let cfg = MachineConfig {
        cpu_count: 0,
        ..Default::default()
    };

    let err = match Machine::new(cfg) {
        Ok(_) => panic!("cpu_count=0 should be rejected"),
        Err(e) => e,
    };
    assert!(matches!(err, MachineError::InvalidCpuCount(0)));

    let msg = err.to_string();
    assert!(
        msg.contains("cpu_count=0"),
        "error message must include the configured cpu_count; got: {msg}"
    );
    assert!(
        msg.contains("SMP is still bring-up only"),
        "error message must explain that SMP is still bring-up only; got: {msg}"
    );
    assert!(
        msg.contains("docs/21-smp.md"),
        "error message must point to relevant docs; got: {msg}"
    );
    assert!(
        msg.contains("docs/09-bios-firmware.md"),
        "error message must also point to the firmware SMP boot docs; got: {msg}"
    );
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
fn enable_ehci_requires_enable_pc_platform() {
    let cfg = MachineConfig {
        enable_pc_platform: false,
        enable_ehci: true,
        ..Default::default()
    };

    assert!(matches!(
        Machine::new(cfg),
        Err(MachineError::EhciRequiresPcPlatform)
    ));
}

#[test]
fn enable_xhci_requires_enable_pc_platform() {
    let cfg = MachineConfig {
        enable_pc_platform: false,
        enable_xhci: true,
        ..Default::default()
    };

    let err = match Machine::new(cfg) {
        Ok(_) => panic!("xhci without pc platform must be rejected"),
        Err(e) => e,
    };
    assert!(matches!(err, MachineError::XhciRequiresPcPlatform));
    assert!(
        err.to_string()
            .contains("enable_xhci requires enable_pc_platform=true"),
        "unexpected error message: {err}"
    );
}

#[test]
fn enable_aerogpu_requires_enable_pc_platform() {
    let cfg = MachineConfig {
        enable_pc_platform: false,
        enable_aerogpu: true,
        enable_vga: false,
        ..Default::default()
    };

    let err = match Machine::new(cfg) {
        Ok(_) => panic!("aerogpu without pc platform must be rejected"),
        Err(e) => e,
    };
    assert!(matches!(err, MachineError::AeroGpuRequiresPcPlatform));
    assert!(
        err.to_string()
            .contains("enable_aerogpu requires enable_pc_platform=true"),
        "unexpected error message: {err}"
    );
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
fn enable_virtio_input_requires_enable_pc_platform() {
    let cfg = MachineConfig {
        enable_pc_platform: false,
        enable_virtio_input: true,
        ..Default::default()
    };

    assert!(matches!(
        Machine::new(cfg),
        Err(MachineError::VirtioInputRequiresPcPlatform)
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
fn win7_storage_preset_is_valid_and_stable() {
    let cfg = MachineConfig::win7_storage(2 * 1024 * 1024);
    assert!(cfg.enable_pc_platform);

    // Win7 storage preset should use the transitional VGA path by default.
    assert!(cfg.enable_vga);
    assert!(!cfg.enable_aerogpu);

    Machine::new(cfg).expect("MachineConfig::win7_storage should pass Machine::new validation");
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
    assert!(!cfg.enable_xhci);

    // Deterministic core devices.
    assert!(!cfg.enable_vga);
    assert!(cfg.enable_aerogpu);
    assert!(cfg.enable_serial);
    assert!(cfg.enable_i8042);
    assert!(cfg.enable_a20_gate);
    assert!(cfg.enable_reset_ctrl);

    Machine::new(cfg).expect("MachineConfig::browser_defaults should pass Machine::new validation");
}

#[test]
fn cpu_by_index_nonzero_is_accessible_when_cpu_count_gt_1() {
    // `cpu_count > 1` is supported for SMP bring-up tests. The canonical machine exposes AP vCPUs
    // via `cpu_by_index` so tests can inspect per-vCPU state deterministically.
    let machine = Machine::new(MachineConfig {
        cpu_count: 2,
        ..Default::default()
    })
    .unwrap();

    // vCPU1 (APIC ID 1) should exist and start in a halted wait-for-SIPI state.
    let ap = machine.cpu_by_index(1);
    assert!(ap.halted, "expected AP to start halted waiting for SIPI");
}

#[test]
fn cpu_by_index_out_of_range_panics_with_message() {
    let machine = Machine::new(MachineConfig {
        cpu_count: 2,
        ..Default::default()
    })
    .unwrap();

    // APs begin in a halted wait-for-SIPI state.
    assert!(machine.cpu_by_index(1).halted);

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = machine.cpu_by_index(2);
    }));

    let err = result.expect_err("expected cpu_by_index(2) to panic");
    let msg = if let Some(s) = err.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = err.downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic payload>".to_string()
    };

    assert!(
        msg.contains("out of range"),
        "message should mention range: {msg}"
    );
    assert!(
        msg.contains("cpu_count"),
        "message should mention cpu_count: {msg}"
    );
}

#[test]
fn cpu_by_index_nonzero_returns_ap_state() {
    // `cpu_count > 1` is allowed so firmware can advertise a multi-CPU topology.
    // `Machine::cpu_by_index` is a test helper for inspecting per-vCPU state deterministically.
    let machine = Machine::new(MachineConfig {
        cpu_count: 2,
        ..Default::default()
    })
    .unwrap();

    let ap = machine.cpu_by_index(1);
    assert!(
        ap.halted,
        "APs should power up in a halted wait-for-SIPI state"
    );
    assert_eq!(
        ap.msr.apic_base & (1 << 8),
        0,
        "BSP bit must be clear for application processors"
    );
}

#[test]
fn cpu_by_index_nonzero_exposes_ap_state_when_smp_is_enabled() {
    let machine = Machine::new(MachineConfig {
        cpu_count: 2,
        ..Default::default()
    })
    .unwrap();

    // APs should start in a halted wait-for-SIPI state.
    let ap = machine.cpu_by_index(1);
    assert!(ap.halted);
}
