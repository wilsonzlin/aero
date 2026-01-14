use aero_devices::pci::profile::AEROGPU_BAR0_INDEX;
use aero_machine::{Machine, MachineConfig, ScanoutSource};
use aero_protocol::aerogpu::aerogpu_pci as pci;
use pretty_assertions::assert_eq;

#[test]
fn aerogpu_cursor_fb_gpa_update_is_atomic_at_hi_commit() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 16 * 1024 * 1024,
        enable_pc_platform: true,
        enable_aerogpu: true,
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        enable_virtio_net: false,
        ..Default::default()
    })
    .unwrap();

    let bdf = m.aerogpu_bdf().expect("AeroGPU device should be present");
    let bar0 = m
        .pci_bar_base(bdf, AEROGPU_BAR0_INDEX)
        .expect("AeroGPU BAR0 should be mapped");

    // Enable PCI bus mastering so scanout reads (treated as DMA) are permitted.
    {
        let pci_cfg = m.pci_config_ports().expect("pc platform enabled");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config_mut(bdf)
            .expect("AeroGPU PCI function missing");
        cfg.set_command(cfg.command() | (1 << 2));
    }

    // One 1x1 scanout buffer (red) plus two distinct 1x1 cursor bitmaps.
    let scanout_fb = 0x0020_0000u64;
    let cursor_a = 0x0020_1000u64;
    let cursor_b = 0x0020_2000u64;
    m.write_physical(scanout_fb, &[0x00, 0x00, 0xFF, 0x00]); // BGRX red
    m.write_physical(cursor_a, &[0xFF, 0x00, 0x00, 0x00]); // BGRX blue
    m.write_physical(cursor_b, &[0x00, 0xFF, 0x00, 0x00]); // BGRX green

    // Configure scanout0 for the red buffer.
    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_WIDTH), 1);
    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_HEIGHT), 1);
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FORMAT),
        pci::AerogpuFormat::B8G8R8X8Unorm as u32,
    );
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_PITCH_BYTES),
        4,
    );
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_LO),
        scanout_fb as u32,
    );
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_HI),
        (scanout_fb >> 32) as u32,
    );
    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_ENABLE), 1);

    assert_eq!(m.active_scanout_source(), ScanoutSource::Wddm);

    // Configure a 1x1 cursor overlay at (0,0) pointing at cursor_a.
    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_CURSOR_X), 0);
    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_CURSOR_Y), 0);
    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_CURSOR_HOT_X), 0);
    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_CURSOR_HOT_Y), 0);
    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_CURSOR_WIDTH), 1);
    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_CURSOR_HEIGHT), 1);
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_CURSOR_FORMAT),
        pci::AerogpuFormat::B8G8R8X8Unorm as u32,
    );
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_CURSOR_PITCH_BYTES),
        4,
    );
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_CURSOR_FB_GPA_LO),
        cursor_a as u32,
    );
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_CURSOR_FB_GPA_HI),
        (cursor_a >> 32) as u32,
    );
    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_CURSOR_ENABLE), 1);

    m.display_present();
    assert_eq!(m.display_resolution(), (1, 1));
    assert_eq!(
        m.display_framebuffer(),
        &[0xFFFF_0000],
        "cursor should fully replace the scanout pixel"
    ); // blue

    // Update only the LO dword to point at cursor_b. Until HI is written, the cursor must remain
    // stable.
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_CURSOR_FB_GPA_LO),
        cursor_b as u32,
    );
    m.display_present();
    assert_eq!(m.display_resolution(), (1, 1));
    assert_eq!(
        m.display_framebuffer(),
        &[0xFFFF_0000],
        "cursor should remain on the previously committed address until CURSOR_FB_GPA_HI is written"
    );

    // Commit by writing HI (still 0 for <4GiB addresses).
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_CURSOR_FB_GPA_HI),
        (cursor_b >> 32) as u32,
    );
    m.display_present();
    assert_eq!(m.display_resolution(), (1, 1));
    assert_eq!(m.display_framebuffer(), &[0xFF00_FF00]); // green
}
