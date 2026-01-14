#![cfg(any(not(target_arch = "wasm32"), target_feature = "atomics"))]

use std::sync::Arc;

use aero_machine::{Machine, MachineConfig};

use aero_devices::pci::profile;
use aero_protocol::aerogpu::aerogpu_pci;
use aero_shared::scanout_state::{ScanoutState, SCANOUT_SOURCE_LEGACY_TEXT, SCANOUT_SOURCE_WDDM};

#[test]
fn aerogpu_scanout_enable_before_fb_is_ok() {
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
    let scanout_state = Arc::new(ScanoutState::new());
    m.set_scanout_state(Some(scanout_state.clone()));

    // Seed a visible legacy VGA text cell so we can detect accidental handoff to a blank WDDM
    // scanout.
    m.write_physical_u16(0xB8000, 0x1F41); // 'A' with bright attribute
    m.display_present();

    let legacy_res = m.display_resolution();
    assert_eq!(legacy_res, (720, 400));
    assert_eq!(scanout_state.snapshot().source, SCANOUT_SOURCE_LEGACY_TEXT);

    let bar0_base = {
        let pci_cfg = m.pci_config_ports().expect("pc platform enabled");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config_mut(profile::AEROGPU.bdf)
            .expect("AeroGPU PCI function missing");
        // Scanout reads behave like device-initiated DMA; enable PCI Bus Master Enable (BME) so the
        // host-side `display_present()` path can legally read from guest RAM.
        cfg.set_command(cfg.command() | (1 << 2));
        cfg.bar_range(0).expect("AeroGPU BAR0 missing").base
    };

    let w = 2u32;
    let h = 2u32;
    let pitch = w * 4;

    // ---------------------------------------------------------------------
    // 1) Win7 KMD behavior: enable scanout while FB_GPA=0 during early init.
    // ---------------------------------------------------------------------
    m.write_physical_u32(
        bar0_base + aerogpu_pci::AEROGPU_MMIO_REG_SCANOUT0_WIDTH as u64,
        w,
    );
    m.write_physical_u32(
        bar0_base + aerogpu_pci::AEROGPU_MMIO_REG_SCANOUT0_HEIGHT as u64,
        h,
    );
    m.write_physical_u32(
        bar0_base + aerogpu_pci::AEROGPU_MMIO_REG_SCANOUT0_FORMAT as u64,
        aerogpu_pci::AerogpuFormat::B8G8R8X8Unorm as u32,
    );
    m.write_physical_u32(
        bar0_base + aerogpu_pci::AEROGPU_MMIO_REG_SCANOUT0_PITCH_BYTES as u64,
        pitch,
    );
    m.write_physical_u32(
        bar0_base + aerogpu_pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_LO as u64,
        0,
    );
    m.write_physical_u32(
        bar0_base + aerogpu_pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_HI as u64,
        0,
    );
    m.write_physical_u32(
        bar0_base + aerogpu_pci::AEROGPU_MMIO_REG_SCANOUT0_ENABLE as u64,
        1,
    );

    // Run the AeroGPU device tick so scanout state updates are published.
    m.process_aerogpu();

    // Scanout must *not* hand off to WDDM yet, since FB_GPA=0 is not a valid config.
    m.display_present();
    assert_eq!(m.display_resolution(), legacy_res);
    assert_eq!(scanout_state.snapshot().source, SCANOUT_SOURCE_LEGACY_TEXT);

    // ---------------------------------------------------------------------
    // 2) Later: driver programs a real framebuffer address while enable stays 1.
    // ---------------------------------------------------------------------
    let fb_gpa: u64 = 0x0008_0000;
    let fb_data: [u8; 16] = [
        // row 0: red, green
        0x00, 0x00, 0xFF, 0x00, // B,G,R,X
        0x00, 0xFF, 0x00, 0x00, //
        // row 1: blue, white
        0xFF, 0x00, 0x00, 0x00, //
        0xFF, 0xFF, 0xFF, 0x00, //
    ];
    m.write_physical(fb_gpa, &fb_data);

    // Update only the framebuffer pointer; `SCANOUT0_ENABLE` remains 1.
    m.write_physical_u32(
        bar0_base + aerogpu_pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_LO as u64,
        fb_gpa as u32,
    );
    m.write_physical_u32(
        bar0_base + aerogpu_pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_HI as u64,
        (fb_gpa >> 32) as u32,
    );

    // Run the device tick to publish the now-valid scanout config.
    m.process_aerogpu();

    m.display_present();

    assert_eq!(m.display_resolution(), (w, h));
    assert_eq!(scanout_state.snapshot().source, SCANOUT_SOURCE_WDDM);

    let expected = [
        u32::from_le_bytes([0xFF, 0x00, 0x00, 0xFF]), // red
        u32::from_le_bytes([0x00, 0xFF, 0x00, 0xFF]), // green
        u32::from_le_bytes([0x00, 0x00, 0xFF, 0xFF]), // blue
        u32::from_le_bytes([0xFF, 0xFF, 0xFF, 0xFF]), // white
    ];
    assert_eq!(m.display_framebuffer(), expected.as_slice());

    // Once WDDM is active, legacy VGA writes must not steal scanout back.
    m.write_physical_u16(0xB8000, 0x2E42); // 'B'
    m.display_present();
    assert_eq!(m.display_resolution(), (w, h));
    assert_eq!(m.display_framebuffer(), expected.as_slice());

    // Explicit scanout disable should return us to legacy.
    m.write_physical_u32(
        bar0_base + aerogpu_pci::AEROGPU_MMIO_REG_SCANOUT0_ENABLE as u64,
        0,
    );
    m.process_aerogpu();
    m.display_present();
    assert_eq!(m.display_resolution(), legacy_res);
    assert_eq!(scanout_state.snapshot().source, SCANOUT_SOURCE_LEGACY_TEXT);
}
