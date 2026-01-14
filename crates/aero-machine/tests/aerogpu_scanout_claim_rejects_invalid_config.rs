#![cfg(any(not(target_arch = "wasm32"), feature = "wasm-threaded"))]

use aero_devices::pci::profile::AEROGPU_BAR0_INDEX;
use aero_machine::{Machine, MachineConfig, ScanoutSource};
use aero_protocol::aerogpu::aerogpu_pci as pci;
use pretty_assertions::assert_ne;

#[test]
fn aerogpu_scanout_claim_rejects_pitch_not_multiple_of_pixel_size() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 8 * 1024 * 1024,
        enable_pc_platform: true,
        enable_aerogpu: true,
        enable_vga: false,
        // Keep the machine minimal/deterministic for the unit test.
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        enable_virtio_net: false,
        ..Default::default()
    })
    .expect("machine should build");

    let bdf = m.aerogpu_bdf().expect("AeroGPU device should be present");
    let bar0 = m
        .pci_bar_base(bdf, AEROGPU_BAR0_INDEX)
        .expect("AeroGPU BAR0 should be assigned by BIOS");
    assert_ne!(bar0, 0);

    let fb_gpa: u64 = 0x0020_0000;
    m.write_physical(fb_gpa, &[0xAA, 0xBB, 0xCC, 0x00]); // BGRX pixel

    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_WIDTH), 1);
    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_HEIGHT), 1);
    // Pitch is large enough, but not a multiple of bytes-per-pixel (4).
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_PITCH_BYTES),
        5,
    );
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FORMAT),
        pci::AerogpuFormat::B8G8R8X8Unorm as u32,
    );
    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_ENABLE), 1);
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_LO),
        fb_gpa as u32,
    );
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_HI),
        (fb_gpa >> 32) as u32,
    );

    // Must not claim the WDDM scanout for an invalid pitch.
    assert_ne!(m.active_scanout_source(), ScanoutSource::Wddm);
    m.process_aerogpu();
    assert_ne!(m.active_scanout_source(), ScanoutSource::Wddm);
}

#[test]
fn aerogpu_scanout_claim_rejects_fb_gpa_overflow() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 8 * 1024 * 1024,
        enable_pc_platform: true,
        enable_aerogpu: true,
        enable_vga: false,
        // Keep the machine minimal/deterministic for the unit test.
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        enable_virtio_net: false,
        ..Default::default()
    })
    .expect("machine should build");

    let bdf = m.aerogpu_bdf().expect("AeroGPU device should be present");
    let bar0 = m
        .pci_bar_base(bdf, AEROGPU_BAR0_INDEX)
        .expect("AeroGPU BAR0 should be assigned by BIOS");
    assert_ne!(bar0, 0);

    // Construct a framebuffer GPA that would overflow when adding scanout bounds.
    let fb_gpa: u64 = u64::MAX - 4;

    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_WIDTH), 1);
    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_HEIGHT), 2);
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_PITCH_BYTES),
        4,
    );
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FORMAT),
        pci::AerogpuFormat::B8G8R8X8Unorm as u32,
    );
    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_ENABLE), 1);
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_LO),
        fb_gpa as u32,
    );
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_HI),
        (fb_gpa >> 32) as u32,
    );

    // Must not claim the WDDM scanout for a config that overflows guest physical address math.
    assert_ne!(m.active_scanout_source(), ScanoutSource::Wddm);
    m.process_aerogpu();
    assert_ne!(m.active_scanout_source(), ScanoutSource::Wddm);
}
