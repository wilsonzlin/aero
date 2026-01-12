use aero_devices::pci::{
    PciBarDefinition, PciBdf, PciBus, PciConfigMechanism1, PciConfigSpace, PciDevice, PciPlatform,
    PciResourceAllocator, PciResourceAllocatorConfig, PCI_CFG_ADDR_PORT, PCI_CFG_DATA_PORT,
};

fn cfg_addr(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    0x8000_0000
        | (u32::from(bus) << 16)
        | (u32::from(device) << 11)
        | (u32::from(function) << 8)
        | u32::from(offset)
}

struct TestBarDevice {
    config: PciConfigSpace,
}

impl TestBarDevice {
    fn new() -> Self {
        let mut config = PciConfigSpace::new(0x1234, 0x5678);
        config.set_class_code(0x02, 0x00, 0x00, 0x00);
        config.set_bar_definition(
            0,
            PciBarDefinition::Mmio32 {
                size: 0x1000,
                prefetchable: false,
            },
        );
        config.set_bar_definition(
            1,
            PciBarDefinition::Mmio32 {
                size: 0x2000,
                prefetchable: false,
            },
        );
        config.set_bar_definition(2, PciBarDefinition::Io { size: 0x20 });
        Self { config }
    }
}

impl PciDevice for TestBarDevice {
    fn config(&self) -> &PciConfigSpace {
        &self.config
    }

    fn config_mut(&mut self) -> &mut PciConfigSpace {
        &mut self.config
    }
}

#[test]
fn platform_devices_enumerate_via_config_mechanism_1() {
    let mut bus = PciPlatform::build_bus();
    let mut cfg = PciConfigMechanism1::new();

    // Read vendor/device of host bridge 00:00.0.
    cfg.io_write(&mut bus, PCI_CFG_ADDR_PORT, 4, cfg_addr(0, 0, 0, 0));
    let id = cfg.io_read(&mut bus, PCI_CFG_DATA_PORT, 4);
    // Note: low 16 bits are vendor, high 16 bits are device.
    assert_eq!(id & 0xFFFF, 0x8086);
    assert_ne!((id >> 16) & 0xFFFF, 0xFFFF);

    // Class code at 0x08: revision/prog_if/subclass/class.
    cfg.io_write(&mut bus, PCI_CFG_ADDR_PORT, 4, cfg_addr(0, 0, 0, 0x08));
    let class = cfg.io_read(&mut bus, PCI_CFG_DATA_PORT, 4);
    let class_code = (class >> 24) as u8;
    let subclass = (class >> 16) as u8;
    assert_eq!(class_code, 0x06);
    assert_eq!(subclass, 0x00);

    // ISA/LPC bridge at 00:1f.0.
    cfg.io_write(&mut bus, PCI_CFG_ADDR_PORT, 4, cfg_addr(0, 0x1f, 0, 0));
    let id = cfg.io_read(&mut bus, PCI_CFG_DATA_PORT, 4);
    assert_eq!(id & 0xFFFF, 0x8086);

    cfg.io_write(&mut bus, PCI_CFG_ADDR_PORT, 4, cfg_addr(0, 0x1f, 0, 0x08));
    let class = cfg.io_read(&mut bus, PCI_CFG_DATA_PORT, 4);
    let class_code = (class >> 24) as u8;
    let subclass = (class >> 16) as u8;
    assert_eq!(class_code, 0x06);
    assert_eq!(subclass, 0x01);
}

#[test]
fn bar_allocation_is_deterministic_and_non_overlapping() {
    let cfg = PciResourceAllocatorConfig {
        mmio_base: 0xE000_0000,
        mmio_size: 0x10_0000,
        io_base: 0x1000,
        io_size: 0x1000,
    };
    let mut allocator = PciResourceAllocator::new(cfg.clone());

    let mut bus = PciBus::new();
    bus.add_device(PciBdf::new(0, 2, 0), Box::new(TestBarDevice::new()));

    bus.reset(&mut allocator)
        .expect("reset should allocate BARs");
    let first_ranges: [Option<_>; 6] = core::array::from_fn(|bar| {
        bus.device_config(PciBdf::new(0, 2, 0))
            .unwrap()
            .bar_range(bar as u8)
    });

    // Reset again and ensure BAR bases match.
    bus.reset(&mut allocator)
        .expect("reset should allocate BARs");
    let second_ranges: [Option<_>; 6] = core::array::from_fn(|bar| {
        bus.device_config(PciBdf::new(0, 2, 0))
            .unwrap()
            .bar_range(bar as u8)
    });

    for bar in 0u8..6u8 {
        assert_eq!(
            first_ranges[bar as usize], second_ranges[bar as usize],
            "BAR{bar} should be deterministic"
        );
    }

    let ranges = first_ranges.into_iter().flatten().collect::<Vec<_>>();
    // Alignment.
    for range in &ranges {
        assert_eq!(range.base % range.size, 0, "{range:?} not aligned");
    }

    // Non-overlap within each address space.
    for i in 0..ranges.len() {
        for j in (i + 1)..ranges.len() {
            let a = ranges[i];
            let b = ranges[j];
            if a.kind != b.kind {
                continue;
            }
            let overlap = a.base < b.end_exclusive() && b.base < a.end_exclusive();
            assert!(!overlap, "ranges overlap: {a:?} vs {b:?}");
        }
    }
}

#[test]
fn bar_reprogramming_updates_decode_ranges() {
    let cfg = PciResourceAllocatorConfig {
        mmio_base: 0xE000_0000,
        mmio_size: 0x10_0000,
        io_base: 0x1000,
        io_size: 0x1000,
    };
    let mut allocator = PciResourceAllocator::new(cfg);

    let mut bus = PciBus::new();
    let dev_addr = PciBdf::new(0, 2, 0);
    bus.add_device(dev_addr, Box::new(TestBarDevice::new()));
    bus.reset(&mut allocator)
        .expect("reset should allocate BARs");

    // Enable memory + I/O decoding.
    bus.write_config(dev_addr, 0x04, 2, 0x0003);
    assert_eq!(bus.mapped_mmio_bars().len(), 2);
    assert_eq!(bus.mapped_io_bars().len(), 1);

    let original = bus.device_config(dev_addr).unwrap().bar_range(0).unwrap();
    let new_base = original.base + 0x20_000;
    assert_eq!(new_base % original.size, 0);

    bus.write_config(dev_addr, 0x10, 4, new_base as u32);
    let mapped = bus
        .mapped_mmio_bars()
        .into_iter()
        .find(|m| m.bdf == dev_addr && m.bar == 0)
        .expect("BAR0 should be mapped after reprogramming");
    assert_eq!(mapped.range.base, new_base);

    // Disable decoding; BAR updates should not create mappings.
    bus.write_config(dev_addr, 0x04, 2, 0x0000);
    assert!(bus.mapped_bars().is_empty());

    bus.write_config(dev_addr, 0x10, 4, (new_base + 0x1000) as u32);
    assert!(bus.mapped_bars().is_empty());

    // Re-enable memory decoding; mapping should appear at the latest base.
    bus.write_config(dev_addr, 0x04, 2, 0x0002);
    let mapped = bus
        .mapped_mmio_bars()
        .into_iter()
        .find(|m| m.bdf == dev_addr && m.bar == 0)
        .expect("BAR0 should be mapped when decoding is enabled");
    assert_eq!(mapped.range.base, new_base + 0x1000);
}
