#![cfg(any(not(target_arch = "wasm32"), feature = "wasm-threaded"))]

use std::sync::Arc;

use aero_machine::{Machine, MachineConfig};
use aero_protocol::aerogpu::aerogpu_pci as pci;
use aero_shared::scanout_state::{ScanoutState, SCANOUT_FORMAT_B8G8R8X8, SCANOUT_SOURCE_WDDM};
use pretty_assertions::assert_eq;

#[test]
fn aerogpu_scanout0_enable_publishes_wddm_scanout_state() {
    let scanout_state = Arc::new(ScanoutState::new());

    let mut m = Machine::new(MachineConfig {
        enable_pc_platform: true,
        enable_aerogpu: true,
        enable_vga: false,
        // Keep the test deterministic.
        enable_serial: false,
        enable_i8042: false,
        ..Default::default()
    })
    .unwrap();

    m.set_scanout_state(Some(scanout_state.clone()));
    m.reset();

    let bdf = m.aerogpu_bdf().expect("AeroGPU device missing");

    // Enable PCI MMIO decode for BAR0.
    let pci_cfg = m.pci_config_ports().expect("PCI config ports missing");
    {
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config_mut(bdf)
            .expect("AeroGPU config missing");
        cfg.set_command(cfg.command() | 0x2);
    }

    let bar0 = m
        .pci_bar_base(bdf, aero_devices::pci::profile::AEROGPU_BAR0_INDEX)
        .expect("AeroGPU BAR0 missing");
    assert!(bar0 != 0, "BAR0 must be assigned a non-zero base");

    // Program scanout0 registers then transition ENABLE 0->1.
    let fb_gpa: u64 = 0x1234_5678_9abc_def0;
    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_WIDTH), 800);
    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_HEIGHT), 600);
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_PITCH_BYTES),
        800 * 4,
    );
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FORMAT),
        pci::AerogpuFormat::B8G8R8X8Unorm as u32,
    );
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_LO),
        fb_gpa as u32,
    );
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_HI),
        (fb_gpa >> 32) as u32,
    );
    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_ENABLE), 1);
    m.process_aerogpu();

    let snap = scanout_state.snapshot();
    assert_eq!(snap.source, SCANOUT_SOURCE_WDDM);
    assert_eq!(snap.base_paddr(), fb_gpa);
    assert_eq!(snap.width, 800);
    assert_eq!(snap.height, 600);
    assert_eq!(snap.pitch_bytes, 800 * 4);
    assert_eq!(snap.format, SCANOUT_FORMAT_B8G8R8X8);
    let gen1 = snap.generation;

    // Disabling scanout is treated as a visibility toggle: publish a disabled WDDM descriptor, but
    // do not allow legacy VGA/VBE to reclaim scanout until reset.
    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_ENABLE), 0);
    m.process_aerogpu();
    let snap2 = scanout_state.snapshot();
    assert_ne!(snap2.generation, gen1);
    assert_eq!(snap2.source, SCANOUT_SOURCE_WDDM);
    assert_eq!(snap2.base_paddr(), 0);
    assert_eq!(snap2.width, 0);
    assert_eq!(snap2.height, 0);
    assert_eq!(snap2.pitch_bytes, 0);
    assert_eq!(snap2.format, SCANOUT_FORMAT_B8G8R8X8);
}
