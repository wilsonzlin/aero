use aero_gpu_vga::VBE_FRAMEBUFFER_OFFSET;
use aero_machine::{Machine, MachineConfig, MachineError};
use aero_pc_constants::PCI_MMIO_BASE;

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
fn vga_lfb_must_fit_inside_pci_mmio_window_when_derived_from_vram_bar_base() {
    // This exercises the derived legacy VGA LFB base path:
    //   lfb_base = vga_vram_bar_base + vga_lfb_offset
    //
    // When the PC platform is enabled, the LFB must be reachable via the PCI MMIO window, so the
    // machine rejects configurations whose (aligned) LFB base falls outside that range.
    // Mirror `Machine::legacy_vga_pci_bar_size_bytes_for_cfg` for the default config.
    let bar_size = aero_gpu_vga::DEFAULT_VRAM_SIZE.max(aero_gpu_vga::VGA_VRAM_SIZE);
    let bar_size = u32::try_from(bar_size).unwrap_or(u32::MAX);
    let bar_size = bar_size
        .max(0x10)
        .checked_next_power_of_two()
        .unwrap_or(0x8000_0000);
    let window_start = u32::try_from(PCI_MMIO_BASE).expect("PCI MMIO base fits in u32");
    // Place the derived LFB base below the PCI MMIO window. Choose an aligned base so alignment
    // masking does not change it.
    let vram_bar_base = window_start.saturating_sub(bar_size) & !(bar_size - 1);
    assert!(u64::from(vram_bar_base) < PCI_MMIO_BASE);

    let lfb_offset = VBE_FRAMEBUFFER_OFFSET as u32;
    let cfg = MachineConfig {
        enable_pc_platform: true,
        enable_vga: true,
        enable_aerogpu: false,
        vga_lfb_base: None,
        vga_vram_bar_base: Some(vram_bar_base),
        vga_lfb_offset: Some(lfb_offset),
        ..Default::default()
    };

    let err = match Machine::new(cfg) {
        Ok(_) => panic!("expected invalid VGA LFB base to be rejected"),
        Err(e) => e,
    };
    let MachineError::VgaLfbOutsidePciMmioWindow {
        requested_base,
        aligned_base,
        size,
    } = err
    else {
        panic!("unexpected error: {err:?}");
    };
    assert_eq!(requested_base, vram_bar_base.wrapping_add(lfb_offset));
    assert_eq!(aligned_base, vram_bar_base);
    assert_eq!(size, bar_size);
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
fn enable_synthetic_usb_hid_requires_enable_uhci() {
    let cfg = MachineConfig {
        enable_pc_platform: true,
        enable_uhci: false,
        enable_synthetic_usb_hid: true,
        ..Default::default()
    };

    let err = match Machine::new(cfg) {
        Ok(_) => panic!("synthetic USB HID without UHCI must be rejected"),
        Err(e) => e,
    };
    assert!(matches!(err, MachineError::SyntheticUsbHidRequiresUhci));
    assert!(
        err.to_string()
            .contains("enable_synthetic_usb_hid requires enable_uhci=true"),
        "unexpected error message: {err}"
    );
}

#[test]
fn enable_aerogpu_requires_enable_pc_platform() {
    let cfg = MachineConfig {
        enable_pc_platform: false,
        enable_aerogpu: true,
        // Keep the failure mode stable (reject for missing PC platform, not VGA conflict) even if
        // validation order evolves.
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
fn vga_lfb_base_must_live_inside_pci_mmio_window_when_pc_platform_enabled() {
    // `0xB000_0000` is the canonical PCIe ECAM base, which is outside the ACPI PCI MMIO BAR window
    // (`0xC000_0000..0xFEC0_0000`). The standalone legacy VGA/VBE LFB must not be placed here when
    // the PC platform is enabled because it would be unreachable via the PCI MMIO router.
    let cfg = MachineConfig {
        enable_pc_platform: true,
        enable_vga: true,
        enable_aerogpu: false,
        vga_lfb_base: Some(0xB000_0000),
        ..Default::default()
    };

    let err = match Machine::new(cfg) {
        Ok(_) => panic!("vga_lfb_base outside PCI MMIO window must be rejected"),
        Err(e) => e,
    };
    assert!(matches!(
        err,
        MachineError::VgaLfbOutsidePciMmioWindow { .. }
    ));
    let msg = err.to_string();
    assert!(
        msg.contains("vga_lfb_base") && msg.contains("PCI MMIO"),
        "unexpected error message: {msg}"
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
fn vga_lfb_base_that_overflows_pci_mmio_window_is_rejected_when_pc_platform_is_enabled() {
    // Pick a base that is inside the PCI MMIO window but whose LFB aperture would cross the end of
    // the window (IOAPIC MMIO begins at 0xFEC0_0000).
    let cfg = MachineConfig {
        enable_pc_platform: true,
        enable_vga: true,
        enable_aerogpu: false,
        vga_lfb_base: Some(0xFE00_0000),
        // Ensure the test remains stable even if `aero_gpu_vga::DEFAULT_VRAM_SIZE` changes.
        vga_vram_size_bytes: Some(16 * 1024 * 1024),
        ..Default::default()
    };

    let err = match Machine::new(cfg) {
        Ok(_) => panic!("expected overflowing vga_lfb_base to be rejected"),
        Err(e) => e,
    };
    assert!(matches!(
        err,
        MachineError::VgaLfbOutsidePciMmioWindow { .. }
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
        "message should mention out-of-range: {msg}"
    );
    assert!(
        msg.contains("cpu_count=2"),
        "message should include cpu_count: {msg}"
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
