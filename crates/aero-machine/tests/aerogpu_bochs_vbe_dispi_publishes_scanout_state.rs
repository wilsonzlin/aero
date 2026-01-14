#![cfg(any(not(target_arch = "wasm32"), feature = "wasm-threaded"))]

use std::sync::Arc;

use aero_machine::{Machine, MachineConfig};
use aero_shared::scanout_state::{
    ScanoutState, SCANOUT_FORMAT_B5G6R5, SCANOUT_FORMAT_B8G8R8X8, SCANOUT_SOURCE_LEGACY_VBE_LFB,
};
use pretty_assertions::assert_eq;

fn base_cfg() -> MachineConfig {
    MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_aerogpu: true,
        enable_vga: false,
        // Keep the test deterministic/minimal.
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        enable_virtio_net: false,
        ..Default::default()
    }
}

#[test]
fn aerogpu_bochs_vbe_dispi_mode_publishes_legacy_vbe_scanout_state() {
    let scanout_state = Arc::new(ScanoutState::new());
    let mut m = Machine::new(base_cfg()).unwrap();
    m.set_scanout_state(Some(scanout_state.clone()));
    m.reset();

    // Program a 64x64x32bpp VBE mode via Bochs VBE_DISPI ports.
    m.io_write(0x01CE, 2, 0x0001);
    m.io_write(0x01CF, 2, 64);
    m.io_write(0x01CE, 2, 0x0002);
    m.io_write(0x01CF, 2, 64);
    m.io_write(0x01CE, 2, 0x0003);
    m.io_write(0x01CF, 2, 32);
    m.io_write(0x01CE, 2, 0x0004);
    m.io_write(0x01CF, 2, 0x0041);

    m.process_aerogpu();

    let snap = scanout_state.snapshot();
    assert_eq!(snap.source, SCANOUT_SOURCE_LEGACY_VBE_LFB);
    assert_eq!(snap.width, 64);
    assert_eq!(snap.height, 64);
    assert_eq!(snap.pitch_bytes, 64 * 4);
    assert_eq!(snap.format, SCANOUT_FORMAT_B8G8R8X8);
    assert_eq!(snap.base_paddr(), m.vbe_lfb_base());
}

#[test]
fn aerogpu_bochs_vbe_dispi_16bpp_offsets_publish_legacy_vbe_scanout_state() {
    let scanout_state = Arc::new(ScanoutState::new());
    let mut m = Machine::new(base_cfg()).unwrap();
    m.set_scanout_state(Some(scanout_state.clone()));
    m.reset();

    // 2x2 visible, 4x4 virtual, 16bpp RGB565 with a (1,1) visible offset.
    m.io_write(0x01CE, 2, 0x0001);
    m.io_write(0x01CF, 2, 2); // xres
    m.io_write(0x01CE, 2, 0x0002);
    m.io_write(0x01CF, 2, 2); // yres
    m.io_write(0x01CE, 2, 0x0003);
    m.io_write(0x01CF, 2, 16); // bpp
    m.io_write(0x01CE, 2, 0x0006);
    m.io_write(0x01CF, 2, 4); // virt_width
    m.io_write(0x01CE, 2, 0x0007);
    m.io_write(0x01CF, 2, 4); // virt_height
    m.io_write(0x01CE, 2, 0x0008);
    m.io_write(0x01CF, 2, 1); // x_offset
    m.io_write(0x01CE, 2, 0x0009);
    m.io_write(0x01CF, 2, 1); // y_offset
    m.io_write(0x01CE, 2, 0x0004);
    m.io_write(0x01CF, 2, 0x0041); // enable + lfb

    m.process_aerogpu();

    let pitch_bytes: u32 = 4 * 2;
    let expected_base = m.vbe_lfb_base() + u64::from(pitch_bytes + 2);

    let snap = scanout_state.snapshot();
    assert_eq!(snap.source, SCANOUT_SOURCE_LEGACY_VBE_LFB);
    assert_eq!(snap.width, 2);
    assert_eq!(snap.height, 2);
    assert_eq!(snap.pitch_bytes, pitch_bytes);
    assert_eq!(snap.format, SCANOUT_FORMAT_B5G6R5);
    assert_eq!(snap.base_paddr(), expected_base);
}
