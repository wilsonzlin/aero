use aero_devices::a20_gate::A20_GATE_PORT;
use aero_devices::pci::profile::AEROGPU;
use aero_machine::{Machine, MachineConfig};
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
fn aerogpu_cursor_is_composited_over_scanout() {
    let mut cfg = MachineConfig {
        ram_size_bytes: 4 * 1024 * 1024,
        enable_pc_platform: true,
        enable_vga: false,
        enable_aerogpu: true,
        enable_serial: false,
        enable_i8042: false,
        enable_reset_ctrl: false,
        // Keep A20 gate enabled so we can place our test framebuffers above 1MiB.
        enable_a20_gate: true,
        ..Default::default()
    };
    // Keep storage off for a minimal test.
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
    let scanout_fb_gpa = 0x0010_0000u64;
    let scanout_pitch = scanout_w * 4;
    // WDDM scanout currently requires a B8G8R8X8-compatible format.
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
    // Configure cursor: 2x2 RGBA with mixed alpha.
    // Placed at (1,1) with hotspot (0,0) => origin (1,1).
    // ---------------------------------------------------------------------
    let cursor_fb_gpa = 0x0010_2000u64;
    let cursor_w = 2u32;
    let cursor_h = 2u32;
    let cursor_pitch = cursor_w * 4;

    // Layout (top-left origin):
    //   [ red opaque,   green 50% ]
    //   [ transparent,  white opaque ]
    let cursor_bytes: [u8; 16] = [
        255, 0, 0, 255, // red
        0, 255, 0, 128, // green half alpha
        0, 0, 0, 0, // transparent
        255, 255, 255, 255, // white
    ];
    m.write_physical(cursor_fb_gpa, &cursor_bytes);

    m.write_physical_u32(
        bar0_base + u64::from(aerogpu_pci::AEROGPU_MMIO_REG_CURSOR_ENABLE),
        1,
    );
    m.write_physical_u32(
        bar0_base + u64::from(aerogpu_pci::AEROGPU_MMIO_REG_CURSOR_X),
        1,
    );
    m.write_physical_u32(
        bar0_base + u64::from(aerogpu_pci::AEROGPU_MMIO_REG_CURSOR_Y),
        1,
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

    // Present and validate.
    m.display_present();
    assert_eq!(m.display_resolution(), (scanout_w, scanout_h));
    let fb = m.display_framebuffer();
    assert_eq!(fb.len(), (scanout_w * scanout_h) as usize);

    let blue = rgba(0, 0, 255, 255);
    let red = rgba(255, 0, 0, 255);
    let blended_green = rgba(0, 128, 127, 255);
    let white = rgba(255, 255, 255, 255);

    // Row 0: all blue.
    assert_eq!(fb[0], blue);
    assert_eq!(fb[1], blue);
    assert_eq!(fb[2], blue);

    // Row 1: [blue, red, blended].
    assert_eq!(fb[3], blue);
    assert_eq!(fb[4], red);
    assert_eq!(fb[5], blended_green);

    // Row 2: [blue, blue (transparent cursor), white].
    assert_eq!(fb[6], blue);
    assert_eq!(fb[7], blue);
    assert_eq!(fb[8], white);
}
