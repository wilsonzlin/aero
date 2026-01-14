#![cfg(any(not(target_arch = "wasm32"), feature = "wasm-threaded"))]

use aero_devices::a20_gate::A20_GATE_PORT;
use aero_devices::pci::profile::AEROGPU;
use aero_machine::{Machine, MachineConfig, ScanoutSource};
use aero_protocol::aerogpu::aerogpu_pci;
use pretty_assertions::assert_eq;

fn enable_a20(m: &mut Machine) {
    // Fast A20 gate at port 0x92: bit1 enables A20.
    m.io_write(A20_GATE_PORT, 1, 0x02);
}

fn rgba(r: u8, g: u8, b: u8, a: u8) -> u32 {
    u32::from_le_bytes([r, g, b, a])
}

#[test]
fn aerogpu_cursor_readback_is_capped_to_avoid_unbounded_allocations() {
    let mut cfg = MachineConfig {
        ram_size_bytes: 16 * 1024 * 1024,
        enable_pc_platform: true,
        enable_vga: false,
        enable_aerogpu: true,
        enable_serial: false,
        enable_i8042: false,
        enable_reset_ctrl: false,
        enable_a20_gate: true,
        ..Default::default()
    };

    // Keep the machine minimal and deterministic for this unit test.
    cfg.enable_ahci = false;
    cfg.enable_ide = false;
    cfg.enable_nvme = false;
    cfg.enable_uhci = false;
    cfg.enable_virtio_blk = false;
    cfg.enable_e1000 = false;
    cfg.enable_virtio_net = false;

    let mut m = Machine::new(cfg).unwrap();
    enable_a20(&mut m);

    // Locate AeroGPU BAR0.
    let (bar0_base, command) = {
        let pci_cfg = m.pci_config_ports().expect("pc platform enabled");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let bus = pci_cfg.bus_mut();
        let cfg = bus
            .device_config(AEROGPU.bdf)
            .expect("AeroGPU device present");
        let bar0_base = cfg.bar_range(0).expect("BAR0 range").base;
        let command = cfg.command();
        (bar0_base, command)
    };

    // Enable bus mastering (COMMAND.BME) so `display_present()` is allowed to DMA from guest RAM.
    {
        let pci_cfg = m.pci_config_ports().expect("pc platform enabled");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let bus = pci_cfg.bus_mut();
        // bit0 = IO space, bit1 = memory space, bit2 = bus master
        bus.write_config(AEROGPU.bdf, 0x04, 2, u32::from(command | 0x6));
    }

    // ---------------------------------------------------------------------
    // Configure scanout0: 3x3 solid blue.
    // ---------------------------------------------------------------------
    let scanout_w = 3u32;
    let scanout_h = 3u32;
    let scanout_fb_gpa = 0x0020_0000u64;
    let scanout_pitch = scanout_w * 4;
    // Fill the framebuffer with solid blue pixels (B,G,R,X = 255,0,0,0).
    let scanout_bytes = [255u8, 0, 0, 0].repeat((scanout_w * scanout_h) as usize);
    m.write_physical(scanout_fb_gpa, &scanout_bytes);

    m.write_physical_u32(
        bar0_base + u64::from(aerogpu_pci::AEROGPU_MMIO_REG_SCANOUT0_ENABLE),
        1,
    );
    m.write_physical_u32(
        bar0_base + u64::from(aerogpu_pci::AEROGPU_MMIO_REG_SCANOUT0_WIDTH),
        scanout_w,
    );
    m.write_physical_u32(
        bar0_base + u64::from(aerogpu_pci::AEROGPU_MMIO_REG_SCANOUT0_HEIGHT),
        scanout_h,
    );
    m.write_physical_u32(
        bar0_base + u64::from(aerogpu_pci::AEROGPU_MMIO_REG_SCANOUT0_FORMAT),
        aerogpu_pci::AerogpuFormat::B8G8R8X8Unorm as u32,
    );
    m.write_physical_u32(
        bar0_base + u64::from(aerogpu_pci::AEROGPU_MMIO_REG_SCANOUT0_PITCH_BYTES),
        scanout_pitch,
    );
    m.write_physical_u32(
        bar0_base + u64::from(aerogpu_pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_LO),
        scanout_fb_gpa as u32,
    );
    m.write_physical_u32(
        bar0_base + u64::from(aerogpu_pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_HI),
        (scanout_fb_gpa >> 32) as u32,
    );

    // ---------------------------------------------------------------------
    // Configure cursor: slightly above the host readback cap.
    // ---------------------------------------------------------------------
    //
    // The cursor readback helper is capped at 1,048,576 pixels (4MiB) to avoid unbounded
    // allocations. Configure a cursor that exceeds this by one row and place an opaque pixel at the
    // top-left so we can detect if it is (incorrectly) composited.
    let cursor_fb_gpa = 0x0040_0000u64;
    let cursor_w = 1024u32;
    let cursor_h = 1025u32;
    let cursor_pitch = cursor_w * 4;

    // Write only the first row; if the cap is bypassed, the first pixel would show up as red.
    let mut cursor_row0 = vec![0u8; cursor_pitch as usize];
    cursor_row0[0] = 255; // R
    cursor_row0[3] = 255; // A
    m.write_physical(cursor_fb_gpa, &cursor_row0);

    m.write_physical_u32(
        bar0_base + u64::from(aerogpu_pci::AEROGPU_MMIO_REG_CURSOR_ENABLE),
        1,
    );
    m.write_physical_u32(
        bar0_base + u64::from(aerogpu_pci::AEROGPU_MMIO_REG_CURSOR_X),
        0,
    );
    m.write_physical_u32(
        bar0_base + u64::from(aerogpu_pci::AEROGPU_MMIO_REG_CURSOR_Y),
        0,
    );
    m.write_physical_u32(
        bar0_base + u64::from(aerogpu_pci::AEROGPU_MMIO_REG_CURSOR_HOT_X),
        0,
    );
    m.write_physical_u32(
        bar0_base + u64::from(aerogpu_pci::AEROGPU_MMIO_REG_CURSOR_HOT_Y),
        0,
    );
    m.write_physical_u32(
        bar0_base + u64::from(aerogpu_pci::AEROGPU_MMIO_REG_CURSOR_WIDTH),
        cursor_w,
    );
    m.write_physical_u32(
        bar0_base + u64::from(aerogpu_pci::AEROGPU_MMIO_REG_CURSOR_HEIGHT),
        cursor_h,
    );
    m.write_physical_u32(
        bar0_base + u64::from(aerogpu_pci::AEROGPU_MMIO_REG_CURSOR_FORMAT),
        aerogpu_pci::AerogpuFormat::R8G8B8A8Unorm as u32,
    );
    m.write_physical_u32(
        bar0_base + u64::from(aerogpu_pci::AEROGPU_MMIO_REG_CURSOR_PITCH_BYTES),
        cursor_pitch,
    );
    m.write_physical_u32(
        bar0_base + u64::from(aerogpu_pci::AEROGPU_MMIO_REG_CURSOR_FB_GPA_LO),
        cursor_fb_gpa as u32,
    );
    m.write_physical_u32(
        bar0_base + u64::from(aerogpu_pci::AEROGPU_MMIO_REG_CURSOR_FB_GPA_HI),
        (cursor_fb_gpa >> 32) as u32,
    );

    // Present and validate scanout is still shown, with cursor ignored due to cap.
    m.display_present();
    assert_eq!(m.active_scanout_source(), ScanoutSource::Wddm);
    assert_eq!(m.display_resolution(), (scanout_w, scanout_h));

    let fb = m.display_framebuffer();
    assert_eq!(fb.len(), (scanout_w * scanout_h) as usize);

    let blue = rgba(0, 0, 255, 255);
    for &px in fb {
        assert_eq!(px, blue);
    }
}
