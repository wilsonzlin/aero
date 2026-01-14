use aero_devices::pci::profile::{
    AEROGPU, AHCI_ABAR_BAR_INDEX, NVME_CONTROLLER, SATA_AHCI_ICH9, VIRTIO_BLK, VIRTIO_NET,
};
use aero_devices::pci::{
    bios_post_with_extra_reservations, PciBarKind, PciBarRange, PciBus, PciConfigSpace, PciDevice,
    PciResourceAllocator, PciResourceAllocatorConfig,
};

const LEGACY_VBE_LFB_RESERVATION_SIZE_BYTES: u64 = 16 * 1024 * 1024;

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
    // Regression test: A large AeroGPU VRAM BAR (BAR1) combined with a fixed legacy VGA/VBE LFB
    // aperture at the start of the PCI MMIO window can cause severe alignment fragmentation,
    // exhausting the default 256MiB PCI MMIO window and making BIOS BAR assignment fail with
    // `OutOfMmioSpace`.
    //
    // This test constructs a realistic "maximal" PCI set:
    // - fixed legacy VGA/VBE LFB reservation at the start of the PCI MMIO window,
    // - AeroGPU identity (00:07.0),
    // - AHCI + NVMe,
    // - virtio-net + virtio-blk (devices after AeroGPU in BDF order).
    //
    // It then asserts BIOS POST succeeds, all expected BAR bases are non-zero, and none overlap
    // the reserved legacy LFB region.
    let alloc_cfg = PciResourceAllocatorConfig::default();

    let mut bus = PciBus::new();

    let legacy_lfb = PciBarRange {
        kind: PciBarKind::Mmio32,
        base: alloc_cfg.mmio_base,
        size: LEGACY_VBE_LFB_RESERVATION_SIZE_BYTES,
    };

    // Canonical chipset/storage/network devices.
    bus.add_device(SATA_AHCI_ICH9.bdf, config_only(SATA_AHCI_ICH9));
    bus.add_device(NVME_CONTROLLER.bdf, config_only(NVME_CONTROLLER));
    bus.add_device(AEROGPU.bdf, config_only(AEROGPU));
    bus.add_device(VIRTIO_NET.bdf, config_only(VIRTIO_NET));
    bus.add_device(VIRTIO_BLK.bdf, config_only(VIRTIO_BLK));

    let mut allocator = PciResourceAllocator::new(alloc_cfg.clone());
    bios_post_with_extra_reservations(&mut bus, &mut allocator, [legacy_lfb].into_iter())
        .expect("bios_post should succeed under default MMIO window");

    let reserved_end = legacy_lfb.end_exclusive();

    // AHCI ABAR.
    let ahci_abar = bus
        .device_config(SATA_AHCI_ICH9.bdf)
        .and_then(|cfg| cfg.bar_range(AHCI_ABAR_BAR_INDEX))
        .expect("AHCI ABAR BAR range missing");
    assert_ne!(ahci_abar.base, 0);
    assert!(
        !(legacy_lfb.base < ahci_abar.end_exclusive() && ahci_abar.base < reserved_end),
        "AHCI ABAR overlaps reserved legacy LFB range: {legacy_lfb:?} vs {ahci_abar:?}"
    );

    // NVMe BAR0.
    let nvme_bar0 = bus
        .device_config(NVME_CONTROLLER.bdf)
        .and_then(|cfg| cfg.bar_range(0))
        .expect("NVMe BAR0 range missing");
    assert_ne!(nvme_bar0.base, 0);
    assert!(
        !(legacy_lfb.base < nvme_bar0.end_exclusive() && nvme_bar0.base < reserved_end),
        "NVMe BAR0 overlaps reserved legacy LFB range: {legacy_lfb:?} vs {nvme_bar0:?}"
    );

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
    for (name, range) in [("AeroGPU BAR0", aerogpu_bar0), ("AeroGPU BAR1", aerogpu_bar1)] {
        assert!(
            !(legacy_lfb.base < range.end_exclusive() && range.base < reserved_end),
            "{name} overlaps reserved legacy LFB range: {legacy_lfb:?} vs {range:?}"
        );
    }

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
        assert!(
            !(legacy_lfb.base < bar0.end_exclusive() && bar0.base < reserved_end),
            "{name} BAR0 overlaps reserved legacy LFB range: {legacy_lfb:?} vs {bar0:?}"
        );
    }
}
