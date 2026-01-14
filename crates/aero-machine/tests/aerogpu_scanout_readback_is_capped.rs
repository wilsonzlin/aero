#![cfg(any(not(target_arch = "wasm32"), target_feature = "atomics"))]

use aero_devices::pci::profile;
use aero_machine::{Machine, MachineConfig, ScanoutSource};
use aero_protocol::aerogpu::aerogpu_pci;
use pretty_assertions::assert_eq;

#[test]
fn aerogpu_scanout_readback_is_capped_to_avoid_unbounded_allocations() {
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

    // Establish a known legacy baseline so we can verify the WDDM claim is released after disable.
    m.write_physical_u16(0xB8000, 0x1F41); // 'A'
    m.display_present();
    let legacy_res = m.display_resolution();
    assert_eq!(legacy_res, (720, 400));
    assert_eq!(m.active_scanout_source(), ScanoutSource::LegacyText);

    let bar0_base = {
        let pci_cfg = m.pci_config_ports().expect("pc platform enabled");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config_mut(profile::AEROGPU.bdf)
            .expect("AeroGPU PCI function missing");
        // Host-side scanout reads behave like device DMA and are gated on PCI bus mastering.
        cfg.set_command(cfg.command() | (1 << 2)); // COMMAND.BME
        cfg.bar_range(0).expect("AeroGPU BAR0 missing").base
    };
    assert_ne!(bar0_base, 0);

    // A scanout slightly larger than the host readback cap (4096*4096 RGBA pixels = 64MiB).
    let width = 4096u32;
    let height = 4097u32;
    let pitch = width * 4;
    let fb_gpa: u64 = 0x0020_0000;

    m.write_physical_u32(bar0_base + aerogpu_pci::AEROGPU_MMIO_REG_SCANOUT0_WIDTH as u64, width);
    m.write_physical_u32(bar0_base + aerogpu_pci::AEROGPU_MMIO_REG_SCANOUT0_HEIGHT as u64, height);
    m.write_physical_u32(
        bar0_base + aerogpu_pci::AEROGPU_MMIO_REG_SCANOUT0_PITCH_BYTES as u64,
        pitch,
    );
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
    m.write_physical_u32(bar0_base + aerogpu_pci::AEROGPU_MMIO_REG_SCANOUT0_ENABLE as u64, 1);

    // The scanout is valid enough to claim WDDM ownership, but the host readback path must reject
    // it to avoid allocating an enormous temporary buffer.
    m.display_present();
    assert_eq!(m.active_scanout_source(), ScanoutSource::Wddm);
    assert_eq!(m.display_resolution(), (0, 0));
    assert!(m.display_framebuffer().is_empty());

    // Explicit disable releases WDDM ownership so legacy scanout becomes visible again.
    m.write_physical_u32(bar0_base + aerogpu_pci::AEROGPU_MMIO_REG_SCANOUT0_ENABLE as u64, 0);
    m.process_aerogpu();
    m.display_present();
    assert_eq!(m.active_scanout_source(), ScanoutSource::LegacyText);
    assert_eq!(m.display_resolution(), legacy_res);
    assert!(!m.display_framebuffer().is_empty());

    // Reset returns scanout ownership to legacy.
    m.reset();
    m.write_physical_u16(0xB8000, 0x1F41); // 'A'
    m.display_present();
    assert_eq!(m.active_scanout_source(), ScanoutSource::LegacyText);
    assert_eq!(m.display_resolution(), legacy_res);
}
