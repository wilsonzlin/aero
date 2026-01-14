use aero_devices::pci::profile::AEROGPU_BAR0_INDEX;
use aero_machine::{Machine, MachineConfig, ScanoutSource};
use aero_protocol::aerogpu::aerogpu_pci as pci;
use pretty_assertions::assert_eq;

#[test]
fn aerogpu_snapshot_preserves_pending_cursor_fb_gpa_lo_until_hi_commit() {
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
    let bdf = vm
        .aerogpu_bdf()
        .expect("AeroGPU device should be present");
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

    // 1x1 scanout buffer + two 1x1 cursor buffers.
    let scanout_fb = 0x0020_0000u64;
    let cursor_a = 0x0020_1000u64;
    let cursor_b = 0x0020_2000u64;
    vm.write_physical(scanout_fb, &[0x00, 0x00, 0xFF, 0x00]); // BGRX red
    vm.write_physical(cursor_a, &[0xFF, 0x00, 0x00, 0x00]); // BGRX blue
    vm.write_physical(cursor_b, &[0x00, 0xFF, 0x00, 0x00]); // BGRX green

    // Configure scanout0 so `display_present` uses the AeroGPU scanout path.
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
        scanout_fb as u32,
    );
    vm.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_HI),
        (scanout_fb >> 32) as u32,
    );
    vm.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_ENABLE), 1);

    assert_eq!(vm.active_scanout_source(), ScanoutSource::Wddm);

    // Configure an initial cursor (blue).
    vm.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_CURSOR_X), 0);
    vm.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_CURSOR_Y), 0);
    vm.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_CURSOR_HOT_X), 0);
    vm.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_CURSOR_HOT_Y), 0);
    vm.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_CURSOR_WIDTH), 1);
    vm.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_CURSOR_HEIGHT), 1);
    vm.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_CURSOR_FORMAT),
        pci::AerogpuFormat::B8G8R8X8Unorm as u32,
    );
    vm.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_CURSOR_PITCH_BYTES),
        4,
    );
    vm.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_CURSOR_FB_GPA_LO),
        cursor_a as u32,
    );
    vm.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_CURSOR_FB_GPA_HI),
        (cursor_a >> 32) as u32,
    );
    vm.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_CURSOR_ENABLE), 1);

    vm.display_present();
    assert_eq!(vm.display_framebuffer(), &[0xFFFF_0000]); // blue cursor over red scanout

    // Start a cursor FB_GPA update by writing only LO.
    vm.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_CURSOR_FB_GPA_LO),
        cursor_b as u32,
    );
    assert_eq!(
        vm.read_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_CURSOR_FB_GPA_LO)),
        cursor_b as u32,
        "LO register readback should reflect the pending write"
    );

    // Cursor base should remain stable until the HI write commits.
    vm.display_present();
    assert_eq!(
        vm.display_framebuffer(),
        &[0xFFFF_0000],
        "cursor should keep using the committed base until the HI write commits"
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
        restored.read_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_CURSOR_FB_GPA_LO)),
        cursor_b as u32,
        "pending LO write should survive snapshot restore so a subsequent HI write commits correctly"
    );

    restored.display_present();
    assert_eq!(
        restored.display_framebuffer(),
        &[0xFFFF_0000],
        "after restore, cursor should still use the committed base until the HI write commits"
    );

    // Commit the update with the HI write. If snapshot restore drops the pending LO value, this
    // write would incorrectly commit the old cursor address.
    restored.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_CURSOR_FB_GPA_HI),
        (cursor_b >> 32) as u32,
    );

    restored.display_present();
    assert_eq!(restored.display_framebuffer(), &[0xFF00_FF00]); // green
}
