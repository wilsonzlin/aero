use aero_devices::a20_gate::A20_GATE_PORT;
use aero_devices::pci::profile;
use aero_machine::{Machine, MachineConfig, ScanoutSource};
use aero_protocol::aerogpu::aerogpu_pci as pci;
use firmware::bda::{BDA_CURSOR_SHAPE_ADDR, BDA_VIDEO_PAGE_OFFSET_ADDR};

fn enable_a20(m: &mut Machine) {
    // Fast A20 gate at port 0x92: bit1 enables A20.
    m.io_write(A20_GATE_PORT, 1, 0x02);
}

#[test]
fn aerogpu_snapshot_preserves_wddm_ownership_when_scanout_disabled() {
    let cfg = MachineConfig {
        enable_pc_platform: true,
        enable_aerogpu: true,
        enable_vga: false,
        // Keep output deterministic.
        enable_serial: false,
        enable_i8042: false,
        ..Default::default()
    };

    let mut m = Machine::new(cfg.clone()).unwrap();

    // Seed a visible legacy text-mode frame so we can detect if snapshot restore incorrectly falls
    // back to legacy output after WDDM has claimed scanout.
    m.write_physical(0xB8000, &vec![0u8; 0x8000]);
    m.write_physical_u16(BDA_VIDEO_PAGE_OFFSET_ADDR, 0);
    // Disable cursor overlay for deterministic output (cursor start CH bit5 = 1).
    m.write_physical_u16(BDA_CURSOR_SHAPE_ADDR, 0x2000);
    m.write_physical_u8(0xB8000, b'A');
    m.write_physical_u8(0xB8001, 0x1F);
    assert_eq!(m.active_scanout_source(), ScanoutSource::LegacyText);
    m.display_present();
    let legacy_res = m.display_resolution();
    assert_ne!(legacy_res, (0, 0));
    assert!(!m.display_framebuffer().is_empty());

    enable_a20(&mut m);

    let bar0_base = {
        let pci_cfg = m.pci_config_ports().expect("pc platform enabled");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config(profile::AEROGPU.bdf)
            .expect("AeroGPU device missing from PCI bus");
        cfg.bar_range(profile::AEROGPU_BAR0_INDEX)
            .expect("AeroGPU BAR0 must exist")
            .base
    };
    assert_ne!(bar0_base, 0);

    // Enable PCI MMIO decode (COMMAND.MEM) + bus mastering (COMMAND.BME).
    {
        let pci_cfg = m.pci_config_ports().expect("pc platform enabled");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let bus = pci_cfg.bus_mut();
        let command = bus.read_config(profile::AEROGPU.bdf, 0x04, 2) as u16;
        bus.write_config(profile::AEROGPU.bdf, 0x04, 2, u32::from(command | 0x0006));
    }

    // Claim WDDM scanout with a small scratch framebuffer in guest RAM.
    let scratch_base = 0x0020_0000u64;
    let width = 64u32;
    let height = 64u32;
    let pitch = width * 4;
    m.write_physical_u32(scratch_base, 0x00CC_BBAA);

    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_WIDTH),
        width,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_HEIGHT),
        height,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_PITCH_BYTES),
        pitch,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_LO),
        scratch_base as u32,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_HI),
        (scratch_base >> 32) as u32,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FORMAT),
        pci::AerogpuFormat::B8G8R8X8Unorm as u32,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_ENABLE),
        1,
    );

    m.display_present();
    assert_eq!(m.active_scanout_source(), ScanoutSource::Wddm);
    assert_eq!(m.display_resolution(), (width, height));
    assert_eq!(m.display_framebuffer()[0], 0xFFAA_BBCC);

    // Disable scanout (visibility toggle). WDDM ownership should remain sticky and blank output.
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_ENABLE),
        0,
    );
    m.process_aerogpu();
    m.display_present();
    assert_eq!(m.active_scanout_source(), ScanoutSource::Wddm);
    assert_eq!(m.display_resolution(), (0, 0));
    assert!(m.display_framebuffer().is_empty());

    // Snapshot/restore should preserve the ownership latch even when scanout is disabled.
    let snap = m.take_snapshot_full().unwrap();

    let mut m2 = Machine::new(cfg).unwrap();
    m2.restore_snapshot_bytes(&snap).unwrap();

    assert_eq!(m2.active_scanout_source(), ScanoutSource::Wddm);
    m2.display_present();
    assert_eq!(m2.display_resolution(), (0, 0));
    assert!(m2.display_framebuffer().is_empty());

    // Legacy text output must remain suppressed after restore while WDDM owns scanout.
    m2.write_physical(0xB8000, &vec![0u8; 0x8000]);
    m2.write_physical_u16(BDA_VIDEO_PAGE_OFFSET_ADDR, 0);
    m2.write_physical_u16(BDA_CURSOR_SHAPE_ADDR, 0x2000);
    m2.write_physical_u8(0xB8000, b'Z');
    m2.write_physical_u8(0xB8001, 0x1F);
    m2.display_present();
    assert_eq!(m2.active_scanout_source(), ScanoutSource::Wddm);
    assert_eq!(m2.display_resolution(), (0, 0));
    assert!(m2.display_framebuffer().is_empty());

    // Reset releases WDDM ownership and reverts to legacy.
    m2.reset();
    m2.write_physical(0xB8000, &vec![0u8; 0x8000]);
    m2.write_physical_u16(BDA_VIDEO_PAGE_OFFSET_ADDR, 0);
    m2.write_physical_u16(BDA_CURSOR_SHAPE_ADDR, 0x2000);
    m2.write_physical_u8(0xB8000, b'R');
    m2.write_physical_u8(0xB8001, 0x1F);
    assert_eq!(m2.active_scanout_source(), ScanoutSource::LegacyText);
    m2.display_present();
    assert_ne!(m2.display_resolution(), (0, 0));
    assert!(!m2.display_framebuffer().is_empty());
}
