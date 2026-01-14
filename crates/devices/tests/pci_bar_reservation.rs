use aero_devices::pci::{
    PciBarDefinition, PciBdf, PciBus, PciConfigSpace, PciDevice, PciResourceAllocator,
    PciResourceAllocatorConfig,
};

struct StubPciDevice {
    cfg: PciConfigSpace,
}

impl StubPciDevice {
    fn new_mmio32_bar0(vendor_id: u16, device_id: u16, size: u32, base: u64) -> Self {
        let mut cfg = PciConfigSpace::new(vendor_id, device_id);
        cfg.set_bar_definition(
            0,
            PciBarDefinition::Mmio32 {
                size,
                prefetchable: false,
            },
        );
        cfg.set_bar_base(0, base);
        Self { cfg }
    }
}

impl PciDevice for StubPciDevice {
    fn config(&self) -> &PciConfigSpace {
        &self.cfg
    }

    fn config_mut(&mut self) -> &mut PciConfigSpace {
        &mut self.cfg
    }
}

#[test]
fn pci_bus_reset_reserves_fixed_bar_assignments() {
    // Keep the window small so overlap issues are easy to reproduce and to ensure this test fails
    // if `PciBus::reset` stops reserving pre-existing BAR ranges.
    let mut allocator = PciResourceAllocator::new(PciResourceAllocatorConfig {
        mmio_base: 0xE000_0000,
        mmio_size: 0x20_000,
        io_base: 0,
        io_size: 0,
    });

    let mut bus = PciBus::new();

    let dev_a_bdf = PciBdf::new(0, 1, 0);
    let dev_b_bdf = PciBdf::new(0, 2, 0);

    // Device A: fixed MMIO BAR0 mapping (example at 0xE000_0000).
    bus.add_device(
        dev_a_bdf,
        Box::new(StubPciDevice::new_mmio32_bar0(
            0x1234,
            0x0001,
            0x2000,
            0xE000_0000,
        )),
    );

    // Device B: needs a new allocation.
    bus.add_device(
        dev_b_bdf,
        Box::new(StubPciDevice::new_mmio32_bar0(0x1234, 0x0002, 0x1000, 0)),
    );

    bus.reset(&mut allocator)
        .expect("PciBus::reset should allocate BARs without overlap");

    let dev_a_range = bus.device_config(dev_a_bdf).unwrap().bar_range(0).unwrap();
    let dev_b_range = bus.device_config(dev_b_bdf).unwrap().bar_range(0).unwrap();

    assert_eq!(dev_a_range.base, 0xE000_0000);

    // Device B's allocation must not overlap the reserved 0xE000_0000..0xE000_2000 range.
    assert_ne!(dev_b_range.base, 0);
    assert_eq!(dev_b_range.base, 0xE000_2000);
    let overlap = dev_a_range.base < dev_b_range.end_exclusive()
        && dev_b_range.base < dev_a_range.end_exclusive();
    assert!(
        !overlap,
        "ranges overlap: {dev_a_range:?} vs {dev_b_range:?}"
    );

    // BAR bases are required to be aligned to their size.
    assert_eq!(dev_a_range.base % dev_a_range.size, 0);
    assert_eq!(dev_b_range.base % dev_b_range.size, 0);
}
