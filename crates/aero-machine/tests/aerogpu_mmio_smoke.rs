use aero_devices::pci::profile;
use aero_machine::{Machine, MachineConfig};
use aero_protocol::aerogpu::aerogpu_pci as pci;

#[test]
fn aerogpu_mmio_scanout_and_cursor_present() {
    // Keep the topology minimal; this test only needs PCI + AeroGPU.
    let cfg = MachineConfig {
        ram_size_bytes: 16 * 1024 * 1024,
        enable_pc_platform: true,
        enable_aerogpu: true,
        enable_vga: false,

        enable_ahci: false,
        enable_nvme: false,
        enable_ide: false,
        enable_virtio_blk: false,
        enable_uhci: false,
        enable_e1000: false,
        enable_virtio_net: false,

        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    };

    let mut m = Machine::new(cfg).unwrap();

    let bdf = profile::AEROGPU.bdf;

    // Locate BAR0 and enable bus mastering (DMA) so scanout/cursor reads behave like a real PCI
    // device.
    let (bar0_base, command) = {
        let pci_cfg = m.pci_config_ports().expect("pc platform enabled");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let bus = pci_cfg.bus_mut();
        let bar0 = bus.read_config(bdf, 0x10, 4) & 0xffff_fff0;
        assert_ne!(bar0, 0, "expected BIOS to assign a non-zero BAR0");
        let cmd = bus.read_config(bdf, 0x04, 2) as u16;
        (u64::from(bar0), cmd)
    };

    {
        let pci_cfg = m.pci_config_ports().unwrap();
        let mut pci_cfg = pci_cfg.borrow_mut();
        let bus = pci_cfg.bus_mut();
        bus.write_config(bdf, 0x04, 2, u32::from(command | (1 << 2)));
    }

    // Identity registers.
    let magic = m.read_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_MAGIC));
    assert_eq!(magic, pci::AEROGPU_MMIO_MAGIC);
    let abi = m.read_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_ABI_VERSION));
    assert_eq!(abi, pci::AEROGPU_ABI_VERSION_U32);

    // 2x2 scanout in BGRA.
    let fb_gpa = 0x200000u64;
    let scanout_bytes: [u8; 16] = [
        // Row 0: red, green
        0x00, 0x00, 0xFF, 0xFF, // BGRA red
        0x00, 0xFF, 0x00, 0xFF, // BGRA green
        // Row 1: blue, white
        0xFF, 0x00, 0x00, 0xFF, // BGRA blue
        0xFF, 0xFF, 0xFF, 0xFF, // BGRA white
    ];
    m.write_physical(fb_gpa, &scanout_bytes);

    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_WIDTH),
        2,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_HEIGHT),
        2,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FORMAT),
        pci::AerogpuFormat::B8G8R8A8Unorm as u32,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_PITCH_BYTES),
        8,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_LO),
        fb_gpa as u32,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_HI),
        (fb_gpa >> 32) as u32,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_ENABLE),
        1,
    );

    m.display_present();
    assert_eq!(m.display_resolution(), (2, 2));
    assert_eq!(
        m.display_framebuffer(),
        &[
            0xFF00_00FF, // red
            0xFF00_FF00, // green
            0xFFFF_0000, // blue
            0xFFFF_FFFF, // white
        ]
    );

    // Program a 1x1 opaque magenta cursor at (1, 0), replacing the green pixel.
    let cursor_gpa = 0x201000u64;
    let cursor_bytes: [u8; 4] = [0xFF, 0x00, 0xFF, 0xFF]; // RGBA magenta
    m.write_physical(cursor_gpa, &cursor_bytes);

    m.write_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_CURSOR_WIDTH), 1);
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_CURSOR_HEIGHT),
        1,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_CURSOR_FORMAT),
        pci::AerogpuFormat::R8G8B8A8Unorm as u32,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_CURSOR_PITCH_BYTES),
        4,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_CURSOR_FB_GPA_LO),
        cursor_gpa as u32,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_CURSOR_FB_GPA_HI),
        (cursor_gpa >> 32) as u32,
    );
    m.write_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_CURSOR_X), 1);
    m.write_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_CURSOR_Y), 0);
    m.write_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_CURSOR_HOT_X), 0);
    m.write_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_CURSOR_HOT_Y), 0);
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_CURSOR_ENABLE),
        1,
    );

    m.display_present();
    assert_eq!(m.display_resolution(), (2, 2));
    assert_eq!(
        m.display_framebuffer(),
        &[
            0xFF00_00FF, // red
            0xFFFF_00FF, // magenta cursor
            0xFFFF_0000, // blue
            0xFFFF_FFFF, // white
        ]
    );

    // Hotspot/offscreen/alpha blend: program a 2x2 cursor with hotspot (1, 1) at position (0, 0).
    // Only the cursor pixel at (1, 1) should land at the top-left of the scanout.
    let scanout_green: [u8; 16] = [
        0x00, 0xFF, 0x00, 0xFF, // BGRA green
        0x00, 0xFF, 0x00, 0xFF, // BGRA green
        0x00, 0xFF, 0x00, 0xFF, // BGRA green
        0x00, 0xFF, 0x00, 0xFF, // BGRA green
    ];
    m.write_physical(fb_gpa, &scanout_green);

    // Cursor pixels are RGBA. Make only the bottom-right pixel semi-transparent red.
    let cursor2_gpa = 0x202000u64;
    let cursor2_bytes: [u8; 16] = [
        0x00, 0x00, 0x00, 0x00, // (0,0) transparent
        0x00, 0x00, 0x00, 0x00, // (1,0) transparent
        0x00, 0x00, 0x00, 0x00, // (0,1) transparent
        0xFF, 0x00, 0x00, 0x80, // (1,1) red @ alpha=0.5
    ];
    m.write_physical(cursor2_gpa, &cursor2_bytes);

    m.write_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_CURSOR_WIDTH), 2);
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_CURSOR_HEIGHT),
        2,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_CURSOR_FORMAT),
        pci::AerogpuFormat::R8G8B8A8Unorm as u32,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_CURSOR_PITCH_BYTES),
        8,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_CURSOR_FB_GPA_LO),
        cursor2_gpa as u32,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_CURSOR_FB_GPA_HI),
        (cursor2_gpa >> 32) as u32,
    );
    m.write_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_CURSOR_X), 0);
    m.write_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_CURSOR_Y), 0);
    m.write_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_CURSOR_HOT_X), 1);
    m.write_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_CURSOR_HOT_Y), 1);
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_CURSOR_ENABLE),
        1,
    );

    m.display_present();
    assert_eq!(m.display_resolution(), (2, 2));
    assert_eq!(
        m.display_framebuffer(),
        &[
            // blend(red@0.5 over green) = r=128,g=127,b=0 with full alpha
            0xFF00_7F80,
            0xFF00_FF00, // green
            0xFF00_FF00, // green
            0xFF00_FF00, // green
        ]
    );

    // Cursor disabled: should not affect scanout.
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_CURSOR_ENABLE),
        0,
    );
    m.display_present();
    assert_eq!(m.display_resolution(), (2, 2));
    assert_eq!(
        m.display_framebuffer(),
        &[0xFF00_FF00, 0xFF00_FF00, 0xFF00_FF00, 0xFF00_FF00]
    );

    // Zero-sized cursor: cursor.enable=1 but width=0 means the cursor bitmap is ignored.
    m.write_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_CURSOR_WIDTH), 0);
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_CURSOR_ENABLE),
        1,
    );
    m.display_present();
    assert_eq!(m.display_resolution(), (2, 2));
    assert_eq!(
        m.display_framebuffer(),
        &[0xFF00_FF00, 0xFF00_FF00, 0xFF00_FF00, 0xFF00_FF00]
    );
}

#[test]
fn aerogpu_mmio_scanout_rgba_present() {
    // Keep the topology minimal; this test only needs PCI + AeroGPU.
    let cfg = MachineConfig {
        ram_size_bytes: 16 * 1024 * 1024,
        enable_pc_platform: true,
        enable_aerogpu: true,
        enable_vga: false,

        enable_ahci: false,
        enable_nvme: false,
        enable_ide: false,
        enable_virtio_blk: false,
        enable_uhci: false,
        enable_e1000: false,
        enable_virtio_net: false,

        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    };

    let mut m = Machine::new(cfg).unwrap();

    let bdf = profile::AEROGPU.bdf;

    // Locate BAR0 and enable bus mastering (DMA) so scanout reads behave like a real PCI device.
    let (bar0_base, command) = {
        let pci_cfg = m.pci_config_ports().expect("pc platform enabled");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let bus = pci_cfg.bus_mut();
        let bar0 = bus.read_config(bdf, 0x10, 4) & 0xffff_fff0;
        assert_ne!(bar0, 0, "expected BIOS to assign a non-zero BAR0");
        let cmd = bus.read_config(bdf, 0x04, 2) as u16;
        (u64::from(bar0), cmd)
    };

    {
        let pci_cfg = m.pci_config_ports().unwrap();
        let mut pci_cfg = pci_cfg.borrow_mut();
        let bus = pci_cfg.bus_mut();
        bus.write_config(bdf, 0x04, 2, u32::from(command | (1 << 2)));
    }

    // 2x2 scanout in RGBA.
    let fb_gpa = 0x200000u64;
    let scanout_bytes: [u8; 16] = [
        // Row 0: red, green
        0xFF, 0x00, 0x00, 0xFF, // RGBA red
        0x00, 0xFF, 0x00, 0xFF, // RGBA green
        // Row 1: blue, white
        0x00, 0x00, 0xFF, 0xFF, // RGBA blue
        0xFF, 0xFF, 0xFF, 0xFF, // RGBA white
    ];
    m.write_physical(fb_gpa, &scanout_bytes);

    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_WIDTH),
        2,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_HEIGHT),
        2,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FORMAT),
        pci::AerogpuFormat::R8G8B8A8Unorm as u32,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_PITCH_BYTES),
        8,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_LO),
        fb_gpa as u32,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_HI),
        (fb_gpa >> 32) as u32,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_ENABLE),
        1,
    );

    m.display_present();
    assert_eq!(m.display_resolution(), (2, 2));
    assert_eq!(
        m.display_framebuffer(),
        &[
            0xFF00_00FF, // red
            0xFF00_FF00, // green
            0xFFFF_0000, // blue
            0xFFFF_FFFF, // white
        ]
    );
}
