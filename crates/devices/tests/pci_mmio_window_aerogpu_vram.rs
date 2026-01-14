use aero_devices::pci::profile::{
    AEROGPU, AHCI_ABAR_BAR_INDEX, NVME_CONTROLLER, SATA_AHCI_ICH9, VIRTIO_BLK, VIRTIO_NET,
};
use aero_devices::pci::{
    bios_post, PciBarDefinition, PciBdf, PciBus, PciConfigSpace, PciDevice, PciResourceAllocator,
    PciResourceAllocatorConfig,
};

const VGA_PCI_STUB_BDF: PciBdf = PciBdf::new(0, 0x0c, 0);
const VGA_PCI_STUB_VRAM_SIZE: u32 = 16 * 1024 * 1024;

struct ConfigOnlyDevice {
    cfg: PciConfigSpace,
}

impl PciDevice for ConfigOnlyDevice {
    fn config(&self) -> &PciConfigSpace {
        &self.cfg
    }

    fn config_mut(&mut self) -> &mut PciConfigSpace {
        &mut self.cfg
    }
}

fn config_only(profile: aero_devices::pci::profile::PciDeviceProfile) -> Box<dyn PciDevice> {
    Box::new(ConfigOnlyDevice {
        cfg: profile.build_config_space(),
    })
}

#[test]
fn aerogpu_vram_bar_does_not_exhaust_default_pci_mmio_window() {
    // Regression test: A large AeroGPU VRAM BAR (BAR1) combined with the fixed Bochs/VBE VGA stub
    // BAR at 0xE000_0000 can cause severe alignment fragmentation, exhausting the default 256MiB
    // PCI MMIO window and making `bios_post` fail with `OutOfMmioSpace`.
    //
    // This test constructs a realistic "maximal" PCI set:
    // - fixed VGA/VBE stub (00:0c.0) for the SVGA linear framebuffer,
    // - AeroGPU identity (00:07.0),
    // - AHCI + NVMe,
    // - virtio-net + virtio-blk (devices after AeroGPU in BDF order).
    //
    // It then asserts BIOS POST succeeds and all expected BAR bases are non-zero.
    let alloc_cfg = PciResourceAllocatorConfig::default();

    let mut bus = PciBus::new();

    // VGA/VBE PCI stub with a fixed LFB BAR base at the start of the PCI MMIO window (0xE000_0000).
    // This mirrors `aero_machine`'s boot display routing and is critical for reproducing the
    // alignment/fragmentation failure mode.
    let mut vga_cfg = PciConfigSpace::new(0x1234, 0x1111);
    vga_cfg.set_class_code(0x03, 0x00, 0x00, 0x00);
    vga_cfg.set_bar_definition(
        0,
        PciBarDefinition::Mmio32 {
            size: VGA_PCI_STUB_VRAM_SIZE,
            prefetchable: false,
        },
    );
    vga_cfg.set_bar_base(0, alloc_cfg.mmio_base);
    bus.add_device(
        VGA_PCI_STUB_BDF,
        Box::new(ConfigOnlyDevice { cfg: vga_cfg }),
    );

    // Canonical chipset/storage/network devices.
    bus.add_device(SATA_AHCI_ICH9.bdf, config_only(SATA_AHCI_ICH9));
    bus.add_device(NVME_CONTROLLER.bdf, config_only(NVME_CONTROLLER));
    bus.add_device(AEROGPU.bdf, config_only(AEROGPU));
    bus.add_device(VIRTIO_NET.bdf, config_only(VIRTIO_NET));
    bus.add_device(VIRTIO_BLK.bdf, config_only(VIRTIO_BLK));

    let mut allocator = PciResourceAllocator::new(alloc_cfg.clone());
    bios_post(&mut bus, &mut allocator)
        .expect("bios_post should succeed under default MMIO window");

    // VGA stub: fixed BAR must be preserved.
    let vga_bar0 = bus
        .device_config(VGA_PCI_STUB_BDF)
        .and_then(|cfg| cfg.bar_range(0))
        .expect("VGA stub BAR0 range missing");
    assert_eq!(vga_bar0.base, alloc_cfg.mmio_base);

    // AHCI ABAR.
    let ahci_abar = bus
        .device_config(SATA_AHCI_ICH9.bdf)
        .and_then(|cfg| cfg.bar_range(AHCI_ABAR_BAR_INDEX))
        .expect("AHCI ABAR BAR range missing");
    assert_ne!(ahci_abar.base, 0);

    // NVMe BAR0.
    let nvme_bar0 = bus
        .device_config(NVME_CONTROLLER.bdf)
        .and_then(|cfg| cfg.bar_range(0))
        .expect("NVMe BAR0 range missing");
    assert_ne!(nvme_bar0.base, 0);

    // AeroGPU BAR0 (MMIO regs) + BAR1 (VRAM aperture).
    let aerogpu_bar0 = bus
        .device_config(AEROGPU.bdf)
        .and_then(|cfg| cfg.bar_range(0))
        .expect("AeroGPU BAR0 range missing");
    let aerogpu_bar1 = bus
        .device_config(AEROGPU.bdf)
        .and_then(|cfg| cfg.bar_range(1))
        .expect("AeroGPU BAR1 range missing");
    assert_ne!(aerogpu_bar0.base, 0);
    assert_ne!(aerogpu_bar1.base, 0);

    // virtio-net + virtio-blk BAR0.
    for (name, bdf) in [
        ("virtio-net", VIRTIO_NET.bdf),
        ("virtio-blk", VIRTIO_BLK.bdf),
    ] {
        let bar0 = bus
            .device_config(bdf)
            .and_then(|cfg| cfg.bar_range(0))
            .unwrap_or_else(|| panic!("{name} BAR0 range missing"));
        assert_ne!(bar0.base, 0);
    }
}
