#![cfg(any(not(target_arch = "wasm32"), target_feature = "atomics"))]

use std::sync::Arc;

use aero_devices::pci::profile;
use aero_machine::{Machine, MachineConfig};
use aero_protocol::aerogpu::aerogpu_pci;
use aero_shared::cursor_state::{CursorState, CURSOR_FORMAT_B8G8R8A8};

#[test]
fn aerogpu_cursor_state_publishes_after_hi_commit() {
    // Minimal deterministic machine config while still including the PCI bus + AeroGPU device.
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
    let cursor_state = Arc::new(CursorState::new());
    m.set_cursor_state(Some(cursor_state.clone()));

    let snap0 = cursor_state.snapshot();
    assert_eq!(snap0.generation, 0);
    assert_eq!(snap0.enable, 0);
    assert_eq!(snap0.format, CURSOR_FORMAT_B8G8R8A8);

    let bar0_base = {
        let pci_cfg = m.pci_config_ports().expect("pc platform enabled");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config_mut(profile::AEROGPU.bdf)
            .expect("AeroGPU PCI function missing");
        cfg.bar_range(0).expect("AeroGPU BAR0 missing").base
    };

    // Program a cursor config, but write FB_GPA_LO without the matching HI word first. The machine
    // should not publish a torn base address.
    let cursor_gpa: u64 = 0x0008_0000;
    m.write_physical_u32(
        bar0_base + aerogpu_pci::AEROGPU_MMIO_REG_CURSOR_ENABLE as u64,
        1,
    );
    m.write_physical_u32(
        bar0_base + aerogpu_pci::AEROGPU_MMIO_REG_CURSOR_X as u64,
        10,
    );
    m.write_physical_u32(
        bar0_base + aerogpu_pci::AEROGPU_MMIO_REG_CURSOR_Y as u64,
        20,
    );
    m.write_physical_u32(
        bar0_base + aerogpu_pci::AEROGPU_MMIO_REG_CURSOR_HOT_X as u64,
        1,
    );
    m.write_physical_u32(
        bar0_base + aerogpu_pci::AEROGPU_MMIO_REG_CURSOR_HOT_Y as u64,
        2,
    );
    m.write_physical_u32(
        bar0_base + aerogpu_pci::AEROGPU_MMIO_REG_CURSOR_WIDTH as u64,
        32,
    );
    m.write_physical_u32(
        bar0_base + aerogpu_pci::AEROGPU_MMIO_REG_CURSOR_HEIGHT as u64,
        32,
    );
    m.write_physical_u32(
        bar0_base + aerogpu_pci::AEROGPU_MMIO_REG_CURSOR_FORMAT as u64,
        CURSOR_FORMAT_B8G8R8A8,
    );
    m.write_physical_u32(
        bar0_base + aerogpu_pci::AEROGPU_MMIO_REG_CURSOR_PITCH_BYTES as u64,
        32 * 4,
    );
    m.write_physical_u32(
        bar0_base + aerogpu_pci::AEROGPU_MMIO_REG_CURSOR_FB_GPA_LO as u64,
        cursor_gpa as u32,
    );

    m.process_aerogpu();
    let snap1 = cursor_state.snapshot();
    assert_eq!(
        snap1.generation, snap0.generation,
        "cursor state must not publish a torn 64-bit base address (LO without HI)"
    );

    // Commit the upper 32 bits; treat HI as the commit point (matching scanout semantics).
    m.write_physical_u32(
        bar0_base + aerogpu_pci::AEROGPU_MMIO_REG_CURSOR_FB_GPA_HI as u64,
        (cursor_gpa >> 32) as u32,
    );
    m.process_aerogpu();

    let snap2 = cursor_state.snapshot();
    assert_eq!(snap2.generation, snap0.generation.wrapping_add(1));
    assert_eq!(snap2.enable, 1);
    assert_eq!(snap2.x, 10);
    assert_eq!(snap2.y, 20);
    assert_eq!(snap2.hot_x, 1);
    assert_eq!(snap2.hot_y, 2);
    assert_eq!(snap2.width, 32);
    assert_eq!(snap2.height, 32);
    assert_eq!(snap2.pitch_bytes, 32 * 4);
    assert_eq!(snap2.format, CURSOR_FORMAT_B8G8R8A8);
    assert_eq!(snap2.base_paddr(), cursor_gpa);
}
