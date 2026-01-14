use aero_devices::pci::profile::AEROGPU_BAR0_INDEX;
use aero_machine::{Machine, MachineConfig, ScanoutSource};
use aero_protocol::aerogpu::aerogpu_pci as pci;
use pretty_assertions::assert_eq;

#[test]
fn aerogpu_snapshot_preserves_pending_scanout0_fb_gpa_lo_until_hi_commit() {
    let cfg = MachineConfig {
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
    };

    let mut vm = Machine::new(cfg.clone()).unwrap();
    let bdf = vm.aerogpu_bdf().expect("AeroGPU device should be present");
    let bar0 = vm
        .pci_bar_base(bdf, AEROGPU_BAR0_INDEX)
        .expect("AeroGPU BAR0 should be mapped");

    // Enable PCI bus mastering so scanout reads (treated as DMA) are permitted.
    {
        let pci_cfg = vm.pci_config_ports().expect("pc platform enabled");
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
    vm.write_physical(fb_a, &[0x00, 0x00, 0xFF, 0x00]); // BGRX red
    vm.write_physical(fb_b, &[0x00, 0xFF, 0x00, 0x00]); // BGRX green

    // Configure scanout0 for fb_a.
    vm.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_WIDTH), 1);
    vm.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_HEIGHT), 1);
    vm.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FORMAT),
        pci::AerogpuFormat::B8G8R8X8Unorm as u32,
    );
    vm.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_PITCH_BYTES),
        4,
    );
    vm.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_LO),
        fb_a as u32,
    );
    vm.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_HI),
        (fb_a >> 32) as u32,
    );
    vm.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_ENABLE), 1);

    assert_eq!(vm.active_scanout_source(), ScanoutSource::Wddm);
    vm.display_present();
    assert_eq!(vm.display_framebuffer(), &[0xFF00_00FF]); // red

    // Start an FB_GPA update by writing only LO.
    vm.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_LO),
        fb_b as u32,
    );
    assert_eq!(
        vm.read_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_LO)),
        fb_b as u32,
        "LO register readback should reflect the pending write"
    );

    let snap = vm.take_snapshot_full().unwrap();

    let mut restored = Machine::new(cfg).unwrap();
    restored.reset();
    restored.restore_snapshot_bytes(&snap).unwrap();

    let bdf = restored
        .aerogpu_bdf()
        .expect("AeroGPU device should be present");
    let bar0 = restored
        .pci_bar_base(bdf, AEROGPU_BAR0_INDEX)
        .expect("AeroGPU BAR0 should be mapped");

    assert_eq!(restored.active_scanout_source(), ScanoutSource::Wddm);
    assert_eq!(
        restored.read_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_LO)),
        fb_b as u32,
        "pending LO write should survive snapshot restore so a subsequent HI write commits correctly"
    );

    // Commit the update with the HI write. If snapshot restore drops the pending LO value, this
    // write would incorrectly commit the old address.
    restored.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_HI),
        (fb_b >> 32) as u32,
    );

    restored.display_present();
    assert_eq!(restored.display_framebuffer(), &[0xFF00_FF00]); // green
}
