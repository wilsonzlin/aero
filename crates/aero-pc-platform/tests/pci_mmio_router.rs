use aero_devices::pci::{PciBarDefinition, PciBdf, PciBus, PciConfigPorts, PciConfigSpace, PciDevice};
use aero_pc_platform::{PciBarMmioHandler, PciBarMmioRouter};
use memory::Bus;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

fn mask_for_size(size: usize) -> u64 {
    match size {
        0 => 0,
        1 => 0xff,
        2 => 0xffff,
        3 => 0x00ff_ffff,
        4 => 0xffff_ffff,
        5 => 0x0000_ffff_ffff,
        6 => 0x00ff_ffff_ffff,
        7 => 0x00ff_ffff_ffff_ffff,
        _ => u64::MAX,
    }
}

#[derive(Debug)]
struct DummyMmio {
    id: u64,
    regs: BTreeMap<u64, u64>,
    writes: Vec<(u64, usize, u64)>,
}

impl DummyMmio {
    fn new(id: u64) -> Self {
        Self {
            id,
            regs: BTreeMap::new(),
            writes: Vec::new(),
        }
    }
}

impl PciBarMmioHandler for DummyMmio {
    fn read(&mut self, offset: u64, size: usize) -> u64 {
        self.regs.get(&offset).copied().unwrap_or(self.id) & mask_for_size(size)
    }

    fn write(&mut self, offset: u64, size: usize, value: u64) {
        let masked = value & mask_for_size(size);
        self.regs.insert(offset, masked);
        self.writes.push((offset, size, masked));
    }
}

struct StubPciConfigDevice {
    cfg: PciConfigSpace,
}

impl StubPciConfigDevice {
    fn new(vendor: u16, device: u16) -> Self {
        let mut cfg = PciConfigSpace::new(vendor, device);
        cfg.set_bar_definition(
            0,
            PciBarDefinition::Mmio32 {
                size: 0x1000,
                prefetchable: false,
            },
        );
        Self { cfg }
    }
}

impl PciDevice for StubPciConfigDevice {
    fn config(&self) -> &PciConfigSpace {
        &self.cfg
    }

    fn config_mut(&mut self) -> &mut PciConfigSpace {
        &mut self.cfg
    }
}

#[test]
fn pci_mmio_router_dispatches_and_tracks_bar_reprogramming() {
    let mmio_base = 0x8000_0000u64;
    let mmio_size = 0x20_000u64;

    let bdf_a = PciBdf::new(0, 5, 0);
    let bdf_b = PciBdf::new(0, 6, 0);

    let mut bus = PciBus::new();
    bus.add_device(bdf_a, Box::new(StubPciConfigDevice::new(0x1234, 0x0001)));
    bus.add_device(bdf_b, Box::new(StubPciConfigDevice::new(0x1234, 0x0002)));

    let pci_cfg = Rc::new(RefCell::new(PciConfigPorts::with_bus(bus)));

    let bar_a0 = mmio_base;
    let bar_b0 = mmio_base + 0x1000;
    let bar_a0_new = mmio_base + 0x3000;

    {
        let mut ports = pci_cfg.borrow_mut();
        let bus = ports.bus_mut();

        // Enable memory decoding for both devices.
        bus.write_config(bdf_a, 0x04, 2, 0x0002);
        bus.write_config(bdf_b, 0x04, 2, 0x0002);

        // Program BAR0 for both devices.
        bus.write_config(bdf_a, 0x10, 4, bar_a0 as u32);
        bus.write_config(bdf_b, 0x10, 4, bar_b0 as u32);
    }

    let dev_a = Rc::new(RefCell::new(DummyMmio::new(0xAAAA_AAAA)));
    let dev_b = Rc::new(RefCell::new(DummyMmio::new(0xBBBB_BBBB)));

    let mut router = PciBarMmioRouter::new(mmio_base, pci_cfg.clone());
    router.register_shared_handler(bdf_a, 0, dev_a.clone());
    router.register_shared_handler(bdf_b, 0, dev_b.clone());

    let mut mem = Bus::new(0);
    mem.map_mmio(mmio_base, mmio_size, Box::new(router));

    // Read from both devices to ensure dispatch is BAR-based.
    assert_eq!(mem.read(bar_a0, 4), 0xAAAA_AAAA);
    assert_eq!(mem.read(bar_b0, 4), 0xBBBB_BBBB);

    // Writes should go to the correct handler and be readable back.
    mem.write(bar_a0 + 0x20, 4, 0xDEAD_BEEF);
    mem.write(bar_b0 + 0x20, 4, 0x1234_5678);

    assert_eq!(mem.read(bar_a0 + 0x20, 4), 0xDEAD_BEEF);
    assert_eq!(mem.read(bar_b0 + 0x20, 4), 0x1234_5678);

    {
        let dev_a = dev_a.borrow();
        let dev_b = dev_b.borrow();
        assert_eq!(dev_a.writes, vec![(0x20, 4, 0xDEAD_BEEF)]);
        assert_eq!(dev_b.writes, vec![(0x20, 4, 0x1234_5678)]);
    }

    // Reprogram device A's BAR0 and ensure dispatch follows the new mapping.
    {
        let mut ports = pci_cfg.borrow_mut();
        ports.bus_mut().write_config(bdf_a, 0x10, 4, bar_a0_new as u32);
    }

    // Old base should no longer decode.
    assert_eq!(mem.read(bar_a0, 4), 0xFFFF_FFFF);

    // New base should decode and preserve the device's state.
    assert_eq!(mem.read(bar_a0_new, 4), 0xAAAA_AAAA);
    assert_eq!(mem.read(bar_a0_new + 0x20, 4), 0xDEAD_BEEF);

    // Device B remains accessible at its original BAR.
    assert_eq!(mem.read(bar_b0, 4), 0xBBBB_BBBB);
    assert_eq!(mem.read(bar_b0 + 0x20, 4), 0x1234_5678);
}
