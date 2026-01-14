use aero_devices::pci::profile::AEROGPU_BAR0_INDEX;
use aero_machine::{Machine, MachineConfig, ScanoutSource};
use aero_protocol::aerogpu::aerogpu_pci as pci;
use pretty_assertions::assert_eq;

#[test]
fn aerogpu_scanout_fb_gpa_update_is_atomic_at_hi_commit() {
    // Small, deterministic machine that still includes PCI + AeroGPU.
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

    // Two distinct 1x1 scanout buffers.
    let fb_a = 0x0020_0000u64;
    let fb_b = 0x0020_1000u64;
    m.write_physical(fb_a, &[0x00, 0x00, 0xFF, 0x00]); // BGRX red
    m.write_physical(fb_b, &[0x00, 0xFF, 0x00, 0x00]); // BGRX green

    // Configure scanout0 for fb_a.
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
        fb_a as u32,
    );
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_HI),
        (fb_a >> 32) as u32,
    );
    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_ENABLE), 1);

    assert_eq!(m.active_scanout_source(), ScanoutSource::Wddm);
    m.display_present();
    assert_eq!(m.display_resolution(), (1, 1));
    assert_eq!(m.display_framebuffer(), &[0xFF00_00FF]); // red

    // Update only the LO dword to point at fb_b. Until HI is written, scanout must remain stable.
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_LO),
        fb_b as u32,
    );
    m.display_present();
    assert_eq!(m.display_resolution(), (1, 1));
    assert_eq!(
        m.display_framebuffer(),
        &[0xFF00_00FF],
        "scanout should remain on the previously committed address until FB_GPA_HI is written"
    );

    // Commit by writing HI (still 0 for <4GiB addresses).
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_HI),
        (fb_b >> 32) as u32,
    );
    m.display_present();
    assert_eq!(m.display_resolution(), (1, 1));
    assert_eq!(m.display_framebuffer(), &[0xFF00_FF00]); // green
}
