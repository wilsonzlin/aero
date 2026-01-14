use aero_devices::pci::PciBdf;
use aero_machine::{Machine, MachineConfig};
use aero_protocol::aerogpu::aerogpu_pci;
use pretty_assertions::assert_eq;

#[test]
fn aerogpu_snapshot_roundtrip_restores_bar0_regs_and_vram() {
    let cfg = MachineConfig {
        ram_size_bytes: 64 * 1024 * 1024,
        enable_pc_platform: true,
        enable_vga: false,
        enable_aerogpu: true,
        // Keep the machine minimal.
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    };

    let mut vm = Machine::new(cfg.clone()).unwrap();

    // Discover AeroGPU BAR bases via PCI config space (BDF 00:07.0).
    let aerogpu_bdf = PciBdf::new(0, 0x07, 0);
    let (bar0_base, bar1_base) = {
        let pci_cfg = vm.pci_config_ports().expect("pc platform enabled");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let bus = pci_cfg.bus_mut();
        let cfg = bus
            .device_config(aerogpu_bdf)
            .expect("AeroGPU device present");
        let bar0 = cfg.bar_range(0).expect("BAR0 present").base;
        let bar1 = cfg.bar_range(1).expect("BAR1 present").base;
        assert!(bar0 != 0);
        assert!(bar1 != 0);
        (bar0, bar1)
    };

    // Enable PCI bus mastering (COMMAND.BME). The AeroGPU scanout path DMA-reads the framebuffer
    // from BAR1 VRAM, and the machine intentionally gates these reads on BME.
    {
        let pci_cfg = vm.pci_config_ports().expect("pc platform enabled");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let bus = pci_cfg.bus_mut();
        let cfg = bus
            .device_config_mut(aerogpu_bdf)
            .expect("AeroGPU device present");
        cfg.set_command(cfg.command() | (1 << 2));
    }

    // ---------------------------------------------------------------------
    // 1) VRAM contents: write a known pattern through the legacy VGA window.
    // ---------------------------------------------------------------------
    let legacy_base = 0xB8000u64;
    let pattern: Vec<u8> = (0u8..=0xFF).collect();
    vm.write_physical(legacy_base, &pattern);

    // Verify the same bytes are visible through BAR1 (VRAM aperture) at the corresponding offset.
    let bar1_alias = bar1_base + (legacy_base - 0xA0000);
    let via_bar1 = vm.read_physical_bytes(bar1_alias, pattern.len());
    assert_eq!(via_bar1, pattern);

    // ---------------------------------------------------------------------
    // 2) Program scanout registers (BAR0) and populate a tiny framebuffer in VRAM.
    // ---------------------------------------------------------------------
    let fb_base = bar1_base + 0x20_000; // VBE LFB offset within VRAM.
    // Populate pixels in B8G8R8X8 (little-endian u32 = 0x00RRGGBB).
    vm.write_physical_u32(fb_base, 0x00FF_0000); // (0,0) red
    vm.write_physical_u32(fb_base + 4, 0x0000_FF00); // (1,0) green
    vm.write_physical_u32(fb_base + 8, 0x0000_00FF); // (2,0) blue
    vm.write_physical_u32(fb_base + 12, 0x00FF_FFFF); // (3,0) white

    let width = 64u32;
    let height = 64u32;
    let pitch = width * 4;
    vm.write_physical_u32(
        bar0_base + u64::from(aerogpu_pci::AEROGPU_MMIO_REG_SCANOUT0_WIDTH),
        width,
    );
    vm.write_physical_u32(
        bar0_base + u64::from(aerogpu_pci::AEROGPU_MMIO_REG_SCANOUT0_HEIGHT),
        height,
    );
    vm.write_physical_u32(
        bar0_base + u64::from(aerogpu_pci::AEROGPU_MMIO_REG_SCANOUT0_PITCH_BYTES),
        pitch,
    );
    vm.write_physical_u32(
        bar0_base + u64::from(aerogpu_pci::AEROGPU_MMIO_REG_SCANOUT0_FORMAT),
        aerogpu_pci::AerogpuFormat::B8G8R8X8Unorm as u32,
    );
    vm.write_physical_u32(
        bar0_base + u64::from(aerogpu_pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_LO),
        fb_base as u32,
    );
    vm.write_physical_u32(
        bar0_base + u64::from(aerogpu_pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_HI),
        (fb_base >> 32) as u32,
    );
    // Enable scanout (also claims scanout for AeroGPU in the machine's handoff latch).
    vm.write_physical_u32(
        bar0_base + u64::from(aerogpu_pci::AEROGPU_MMIO_REG_SCANOUT0_ENABLE),
        1,
    );

    vm.display_present();
    assert_eq!(vm.display_resolution(), (width, height));
    let fb_before = vm.display_framebuffer().to_vec();
    assert_eq!(fb_before[0], 0xFF00_00FF);
    assert_eq!(fb_before[1], 0xFF00_FF00);
    assert_eq!(fb_before[2], 0xFFFF_0000);
    assert_eq!(fb_before[3], 0xFFFF_FFFF);

    let snap = vm.take_snapshot_full().unwrap();

    // Restore into a fresh machine and validate VRAM + scanout state survives.
    let mut vm2 = Machine::new(cfg).unwrap();
    vm2.reset();
    vm2.restore_snapshot_bytes(&snap).unwrap();

    // Legacy window bytes.
    let after_pattern = vm2.read_physical_bytes(legacy_base, pattern.len());
    assert_eq!(after_pattern, pattern);

    vm2.display_present();
    assert_eq!(vm2.display_resolution(), (width, height));
    let fb_after = vm2.display_framebuffer();
    assert_eq!(fb_after[0..4], fb_before[0..4]);
}
