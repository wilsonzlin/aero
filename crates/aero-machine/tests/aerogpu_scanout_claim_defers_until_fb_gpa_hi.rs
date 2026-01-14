use aero_devices::pci::profile::AEROGPU_BAR0_INDEX;
use aero_machine::{Machine, MachineConfig, ScanoutSource};
use aero_protocol::aerogpu::aerogpu_pci as pci;
use pretty_assertions::assert_eq;

#[test]
fn aerogpu_scanout_claim_defers_until_fb_gpa_hi_is_written() {
    // Use a RAM size above 4GiB so the test can exercise scanouts with a non-zero GPA HI dword
    // without allocating multi-gigabyte buffers (the machine switches to sparse RAM above the
    // configured threshold).
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 5u64 * 1024 * 1024 * 1024,
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
    assert_ne!(bar0, 0, "expected AeroGPU BAR0 base");

    // Host-side scanout reads are treated as device-initiated DMA and are gated by PCI
    // COMMAND.BME.
    {
        let pci_cfg = m.pci_config_ports().expect("pc platform enabled");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config_mut(bdf)
            .expect("AeroGPU PCI function missing");
        cfg.set_command(cfg.command() | (1 << 2));
    }

    // Seed a high-memory scanout framebuffer and point scanout0 at it. Use a tiny 1x1 framebuffer
    // so the test remains cheap even with huge configured RAM.
    let fb_gpa: u64 = 0x1_0000_0000u64 + 0x2000;
    m.write_physical(fb_gpa, &[0xAA, 0xBB, 0xCC, 0x00]); // BGRX pixel

    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_WIDTH), 1);
    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_HEIGHT), 1);
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_PITCH_BYTES),
        4,
    );
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FORMAT),
        pci::AerogpuFormat::B8G8R8X8Unorm as u32,
    );

    // Win7 KMD init sequence: enable scanout early, then program FB_GPA as a pair of 32-bit
    // writes. Claiming WDDM scanout after only the LO write would observe a torn 64-bit address.
    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_ENABLE), 1);
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_LO),
        fb_gpa as u32,
    );

    // Must *not* hand off to WDDM until FB_GPA_HI is written.
    assert_ne!(m.active_scanout_source(), ScanoutSource::Wddm);
    m.display_present();
    assert_ne!(
        m.display_resolution(),
        (1, 1),
        "display_present should not read scanout0 until FB_GPA_HI commits the 64-bit address"
    );

    // Commit the 64-bit framebuffer pointer by writing HI.
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_HI),
        (fb_gpa >> 32) as u32,
    );

    assert_eq!(m.active_scanout_source(), ScanoutSource::Wddm);
    m.display_present();
    assert_eq!(m.display_resolution(), (1, 1));
    assert_eq!(m.display_framebuffer(), &[0xFFAA_BBCC]);
}
