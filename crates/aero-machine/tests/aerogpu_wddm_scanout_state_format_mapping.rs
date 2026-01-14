#![cfg(any(not(target_arch = "wasm32"), feature = "wasm-threaded"))]

use std::sync::Arc;

use aero_devices::pci::profile::AEROGPU_BAR0_INDEX;
use aero_machine::{Machine, MachineConfig};
use aero_protocol::aerogpu::aerogpu_pci as pci;
use aero_shared::scanout_state::{
    ScanoutState, SCANOUT_FORMAT_B5G5R5A1, SCANOUT_FORMAT_B5G6R5, SCANOUT_FORMAT_B8G8R8A8,
    SCANOUT_FORMAT_B8G8R8A8_SRGB, SCANOUT_FORMAT_B8G8R8X8, SCANOUT_FORMAT_B8G8R8X8_SRGB,
    SCANOUT_FORMAT_R8G8B8A8, SCANOUT_FORMAT_R8G8B8A8_SRGB, SCANOUT_FORMAT_R8G8B8X8,
    SCANOUT_FORMAT_R8G8B8X8_SRGB, SCANOUT_SOURCE_LEGACY_TEXT, SCANOUT_SOURCE_WDDM,
};
use pretty_assertions::assert_eq;

fn new_deterministic_aerogpu_machine() -> Machine {
    Machine::new(MachineConfig {
        ram_size_bytes: 16 * 1024 * 1024,
        enable_pc_platform: true,
        enable_aerogpu: true,
        enable_vga: false,
        // Keep the machine minimal and deterministic for this unit test.
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        enable_virtio_net: false,
        ..Default::default()
    })
    .unwrap()
}

#[test]
fn wddm_scanout_state_format_mapping_rejects_unsupported_formats_deterministically() {
    let scanout_state = Arc::new(ScanoutState::new());
    let mut m = new_deterministic_aerogpu_machine();
    m.set_scanout_state(Some(scanout_state.clone()));
    m.reset();

    // Sanity check: reset publishes legacy text scanout.
    let snap = scanout_state.snapshot();
    assert_eq!(snap.source, SCANOUT_SOURCE_LEGACY_TEXT);
    assert_eq!(snap.format, SCANOUT_FORMAT_B8G8R8X8);

    let bdf = m
        .aerogpu_bdf()
        .expect("expected AeroGPU device to be present");
    let bar0 = m
        .pci_bar_base(bdf, AEROGPU_BAR0_INDEX)
        .expect("expected AeroGPU BAR0 to be mapped");
    assert_ne!(bar0, 0, "expected AeroGPU BAR0 to be assigned by BIOS");

    let fb_gpa = 0x0010_0000u64;

    // Program a valid BGRX scanout.
    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_WIDTH), 640);
    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_HEIGHT), 480);
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_PITCH_BYTES),
        640 * 4,
    );
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_LO),
        fb_gpa as u32,
    );
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_HI),
        (fb_gpa >> 32) as u32,
    );
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FORMAT),
        pci::AerogpuFormat::B8G8R8X8Unorm as u32,
    );
    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_ENABLE), 1);

    m.process_aerogpu();

    let snap = scanout_state.snapshot();
    assert_eq!(snap.source, SCANOUT_SOURCE_WDDM);
    assert_eq!(snap.base_paddr(), fb_gpa);
    assert_eq!(snap.width, 640);
    assert_eq!(snap.height, 480);
    assert_eq!(snap.pitch_bytes, 640 * 4);
    assert_eq!(snap.format, SCANOUT_FORMAT_B8G8R8X8);

    // Program a BGRA scanout; this should publish the corresponding scanout format value.
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FORMAT),
        pci::AerogpuFormat::B8G8R8A8Unorm as u32,
    );
    m.process_aerogpu();
    let snap = scanout_state.snapshot();
    assert_eq!(snap.source, SCANOUT_SOURCE_WDDM);
    assert_eq!(snap.base_paddr(), fb_gpa);
    assert_eq!(snap.width, 640);
    assert_eq!(snap.height, 480);
    assert_eq!(snap.pitch_bytes, 640 * 4);
    assert_eq!(snap.format, SCANOUT_FORMAT_B8G8R8A8);

    // Program sRGB variants; these should preserve the sRGB discriminants in the shared state.
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FORMAT),
        pci::AerogpuFormat::B8G8R8X8UnormSrgb as u32,
    );
    m.process_aerogpu();
    let snap = scanout_state.snapshot();
    assert_eq!(snap.format, SCANOUT_FORMAT_B8G8R8X8_SRGB);

    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FORMAT),
        pci::AerogpuFormat::B8G8R8A8UnormSrgb as u32,
    );
    m.process_aerogpu();
    let snap = scanout_state.snapshot();
    assert_eq!(snap.format, SCANOUT_FORMAT_B8G8R8A8_SRGB);

    // Program RGBA/RGBX scanout formats; these should publish the corresponding protocol
    // discriminants in the shared state.
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FORMAT),
        pci::AerogpuFormat::R8G8B8A8Unorm as u32,
    );
    m.process_aerogpu();
    let snap = scanout_state.snapshot();
    assert_eq!(snap.format, SCANOUT_FORMAT_R8G8B8A8);

    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FORMAT),
        pci::AerogpuFormat::R8G8B8X8Unorm as u32,
    );
    m.process_aerogpu();
    let snap = scanout_state.snapshot();
    assert_eq!(snap.format, SCANOUT_FORMAT_R8G8B8X8);

    // sRGB variants should also be preserved.
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FORMAT),
        pci::AerogpuFormat::R8G8B8A8UnormSrgb as u32,
    );
    m.process_aerogpu();
    let snap = scanout_state.snapshot();
    assert_eq!(snap.format, SCANOUT_FORMAT_R8G8B8A8_SRGB);

    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FORMAT),
        pci::AerogpuFormat::R8G8B8X8UnormSrgb as u32,
    );
    m.process_aerogpu();
    let snap = scanout_state.snapshot();
    assert_eq!(snap.format, SCANOUT_FORMAT_R8G8B8X8_SRGB);

    // Program 16bpp scanout formats; these should publish the corresponding protocol discriminants.
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_PITCH_BYTES),
        640 * 2,
    );
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FORMAT),
        pci::AerogpuFormat::B5G6R5Unorm as u32,
    );
    m.process_aerogpu();
    let snap = scanout_state.snapshot();
    assert_eq!(snap.source, SCANOUT_SOURCE_WDDM);
    assert_eq!(snap.base_paddr(), fb_gpa);
    assert_eq!(snap.width, 640);
    assert_eq!(snap.height, 480);
    assert_eq!(snap.pitch_bytes, 640 * 2);
    assert_eq!(snap.format, SCANOUT_FORMAT_B5G6R5);

    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FORMAT),
        pci::AerogpuFormat::B5G5R5A1Unorm as u32,
    );
    m.process_aerogpu();
    let snap = scanout_state.snapshot();
    assert_eq!(snap.source, SCANOUT_SOURCE_WDDM);
    assert_eq!(snap.base_paddr(), fb_gpa);
    assert_eq!(snap.width, 640);
    assert_eq!(snap.height, 480);
    assert_eq!(snap.pitch_bytes, 640 * 2);
    assert_eq!(snap.format, SCANOUT_FORMAT_B5G5R5A1);

    // Program an unsupported scanout format; this must not panic and must publish a deterministic
    // disabled descriptor rather than leaking an unsupported format value to the shared state.
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FORMAT),
        pci::AerogpuFormat::D24UnormS8Uint as u32,
    );
    m.process_aerogpu();

    let snap0 = scanout_state.snapshot();
    assert_eq!(snap0.source, SCANOUT_SOURCE_WDDM);
    assert_eq!(snap0.base_paddr(), 0);
    assert_eq!(snap0.width, 0);
    assert_eq!(snap0.height, 0);
    assert_eq!(snap0.pitch_bytes, 0);
    assert_eq!(snap0.format, SCANOUT_FORMAT_B8G8R8X8);

    // Re-processing without further register writes should not change the snapshot.
    m.process_aerogpu();
    let snap1 = scanout_state.snapshot();
    assert_eq!(snap0, snap1);
}
#[test]
fn wddm_scanout_state_defers_fb_gpa_updates_until_hi_written() {
    let scanout_state = Arc::new(ScanoutState::new());
    let mut m = new_deterministic_aerogpu_machine();
    m.set_scanout_state(Some(scanout_state.clone()));
    m.reset();

    let bdf = m
        .aerogpu_bdf()
        .expect("expected AeroGPU device to be present");
    let bar0 = m
        .pci_bar_base(bdf, AEROGPU_BAR0_INDEX)
        .expect("expected AeroGPU BAR0 to be mapped");

    // Start with a valid scanout.
    let fb0 = 0x0010_0000u64;
    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_WIDTH), 640);
    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_HEIGHT), 480);
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_PITCH_BYTES),
        640 * 4,
    );
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FORMAT),
        pci::AerogpuFormat::B8G8R8X8Unorm as u32,
    );
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_LO),
        fb0 as u32,
    );
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_HI),
        (fb0 >> 32) as u32,
    );
    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_ENABLE), 1);
    m.process_aerogpu();

    let snap0 = scanout_state.snapshot();
    assert_eq!(snap0.source, SCANOUT_SOURCE_WDDM);
    assert_eq!(snap0.base_paddr(), fb0);

    // Update the base address with only the LO write; publishing should be deferred until the HI
    // write arrives so we never publish a torn 64-bit address.
    let fb1 = 0x0020_0000u64;
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_LO),
        fb1 as u32,
    );
    m.process_aerogpu();

    let snap1 = scanout_state.snapshot();
    assert_eq!(snap1.source, SCANOUT_SOURCE_WDDM);
    assert_eq!(
        snap1.base_paddr(),
        fb0,
        "base_paddr must not update until FB_GPA_HI is written"
    );

    // Commit the update by writing HI.
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_HI),
        (fb1 >> 32) as u32,
    );
    m.process_aerogpu();

    let snap2 = scanout_state.snapshot();
    assert_eq!(snap2.source, SCANOUT_SOURCE_WDDM);
    assert_eq!(snap2.base_paddr(), fb1);
}
