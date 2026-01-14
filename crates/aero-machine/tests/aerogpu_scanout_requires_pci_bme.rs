#![cfg(any(not(target_arch = "wasm32"), target_feature = "atomics"))]

use aero_devices::pci::profile;
use aero_machine::{Machine, MachineConfig, ScanoutSource};
use aero_protocol::aerogpu::aerogpu_pci;
use pretty_assertions::assert_eq;

#[test]
fn aerogpu_scanout_requires_pci_bus_master_enable_for_host_reads() {
    // Keep the machine small and deterministic for a unit test while still including the PCI bus
    // and the canonical AeroGPU PCI identity.
    let cfg = MachineConfig {
        ram_size_bytes: 8 * 1024 * 1024,
        enable_pc_platform: true,
        enable_vga: false,
        enable_aerogpu: true,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        enable_virtio_net: false,
        ..Default::default()
    };

    let mut m = Machine::new(cfg).expect("Machine::new should succeed");

    // Seed a visible legacy VGA text cell so we can detect accidental fallback when the guest has
    // already claimed WDDM scanout.
    m.write_physical_u16(0xB8000, 0x1F41); // 'A' with bright attribute
    m.display_present();
    assert_eq!(m.active_scanout_source(), ScanoutSource::LegacyText);
    let legacy_res = m.display_resolution();
    assert_eq!(legacy_res, (720, 400));

    let bar0_base = {
        let pci_cfg = m.pci_config_ports().expect("pc platform enabled");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config_mut(profile::AEROGPU.bdf)
            .expect("AeroGPU PCI function missing");
        cfg.bar_range(0).expect("AeroGPU BAR0 missing").base
    };
    assert_ne!(bar0_base, 0);

    let fb_gpa: u64 = 0x0020_0000;
    let w = 2u32;
    let h = 1u32;
    let pitch = w * 4;

    // Pixel (0,0): B,G,R,X = AA,BB,CC,00.
    m.write_physical(fb_gpa, &[0xAA, 0xBB, 0xCC, 0x00, 0, 0, 0, 0]);

    m.write_physical_u32(bar0_base + aerogpu_pci::AEROGPU_MMIO_REG_SCANOUT0_WIDTH as u64, w);
    m.write_physical_u32(bar0_base + aerogpu_pci::AEROGPU_MMIO_REG_SCANOUT0_HEIGHT as u64, h);
    m.write_physical_u32(bar0_base + aerogpu_pci::AEROGPU_MMIO_REG_SCANOUT0_PITCH_BYTES as u64, pitch);
    m.write_physical_u32(
        bar0_base + aerogpu_pci::AEROGPU_MMIO_REG_SCANOUT0_FORMAT as u64,
        aerogpu_pci::AerogpuFormat::B8G8R8X8Unorm as u32,
    );
    m.write_physical_u32(
        bar0_base + aerogpu_pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_LO as u64,
        fb_gpa as u32,
    );
    m.write_physical_u32(
        bar0_base + aerogpu_pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_HI as u64,
        (fb_gpa >> 32) as u32,
    );
    // Enable scanout, which (once configured validly) claims WDDM scanout.
    m.write_physical_u32(bar0_base + aerogpu_pci::AEROGPU_MMIO_REG_SCANOUT0_ENABLE as u64, 1);

    // ---------------------------------------------------------------------
    // 1) With PCI COMMAND.BME=0, host-side scanout reads must be gated off.
    // ---------------------------------------------------------------------
    m.display_present();
    assert_eq!(m.active_scanout_source(), ScanoutSource::Wddm);
    assert_eq!(m.display_resolution(), (0, 0));
    assert!(m.display_framebuffer().is_empty());

    // ---------------------------------------------------------------------
    // 2) Once the guest enables bus mastering, scanout becomes readable.
    // ---------------------------------------------------------------------
    {
        let pci_cfg = m.pci_config_ports().expect("pc platform enabled");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config_mut(profile::AEROGPU.bdf)
            .expect("AeroGPU PCI function missing");
        cfg.set_command(cfg.command() | (1 << 2)); // COMMAND.BME
    }

    m.display_present();
    assert_eq!(m.active_scanout_source(), ScanoutSource::Wddm);
    assert_eq!(m.display_resolution(), (w, h));
    assert_eq!(m.display_framebuffer()[0], 0xFFAA_BBCC);

    // Explicit disable should release the WDDM scanout claim and fall back to legacy output.
    m.write_physical_u32(bar0_base + aerogpu_pci::AEROGPU_MMIO_REG_SCANOUT0_ENABLE as u64, 0);
    m.process_aerogpu();
    m.display_present();
    assert_eq!(m.active_scanout_source(), ScanoutSource::LegacyText);
    assert_eq!(m.display_resolution(), legacy_res);
}

