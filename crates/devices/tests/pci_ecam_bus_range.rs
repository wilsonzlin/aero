use aero_devices::pci::{
    PciBdf, PciConfigPorts, PciConfigSpace, PciDevice, PciEcamConfig, PciEcamMmio,
    PCIE_ECAM_BUS_STRIDE,
};
use memory::Bus;
use std::cell::RefCell;
use std::rc::Rc;

fn ecam_addr(base: u64, bus: u8, device: u8, function: u8, offset: u16) -> u64 {
    base + (u64::from(bus) << 20)
        + (u64::from(device) << 15)
        + (u64::from(function) << 12)
        + u64::from(offset)
}

#[test]
fn pci_ecam_enforces_configured_bus_range() {
    struct Stub {
        cfg: PciConfigSpace,
    }

    impl PciDevice for Stub {
        fn config(&self) -> &PciConfigSpace {
            &self.cfg
        }

        fn config_mut(&mut self) -> &mut PciConfigSpace {
            &mut self.cfg
        }
    }

    // Build a bus with two devices: one on bus 0 and one on bus 1. The ECAM window we expose below
    // is configured to cover only bus 0.
    let cfg_ports = Rc::new(RefCell::new(PciConfigPorts::new()));
    let bdf0 = PciBdf::new(0, 2, 0);
    let bdf1 = PciBdf::new(1, 2, 0);
    {
        let mut ports = cfg_ports.borrow_mut();
        let bus = ports.bus_mut();

        let mut cfg0 = PciConfigSpace::new(0x1234, 0x5678);
        cfg0.set_command(0x0005);
        bus.add_device(bdf0, Box::new(Stub { cfg: cfg0 }));

        let mut cfg1 = PciConfigSpace::new(0x1111, 0x2222);
        cfg1.set_command(0x0003);
        bus.add_device(bdf1, Box::new(Stub { cfg: cfg1 }));
    }

    let ecam_base = 0xC000_0000;
    let ecam_cfg = PciEcamConfig {
        segment: 0,
        start_bus: 0,
        end_bus: 0,
    };

    // Map *two* buses worth of address space so accesses to bus 1 hit the ECAM handler but should
    // be rejected by bus-range enforcement.
    let mut mem = Bus::new(0);
    mem.map_mmio(
        ecam_base,
        2 * PCIE_ECAM_BUS_STRIDE,
        Box::new(PciEcamMmio::new(cfg_ports.clone(), ecam_cfg)),
    );

    // Bus 0 works: vendor/device ID is visible for the present device.
    let id_bus0 = mem.read(ecam_addr(ecam_base, 0, 2, 0, 0x00), 4) as u32;
    assert_eq!(id_bus0, 0x5678_1234);

    // Bus 1 is outside the configured bus range ([0, 0]) and should float high (open bus).
    let addr_bus1 = ecam_addr(ecam_base, 1, 2, 0, 0x00);
    assert_eq!(mem.read(addr_bus1, 1) as u8, 0xFF);
    assert_eq!(mem.read(addr_bus1, 2) as u16, 0xFFFF);
    assert_eq!(mem.read(addr_bus1, 4) as u32, 0xFFFF_FFFF);

    // Writes to out-of-range buses should be ignored and must not affect any in-range device.
    let (cmd0_before, cmd1_before) = {
        let mut ports = cfg_ports.borrow_mut();
        let bus = ports.bus_mut();
        (
            bus.read_config(bdf0, 0x04, 2) as u16,
            bus.read_config(bdf1, 0x04, 2) as u16,
        )
    };
    assert_eq!(cmd0_before, 0x0005);
    assert_eq!(cmd1_before, 0x0003);

    mem.write(ecam_addr(ecam_base, 1, 2, 0, 0x04), 2, 0x0007);

    let (cmd0_after, cmd1_after) = {
        let mut ports = cfg_ports.borrow_mut();
        let bus = ports.bus_mut();
        (
            bus.read_config(bdf0, 0x04, 2) as u16,
            bus.read_config(bdf1, 0x04, 2) as u16,
        )
    };
    assert_eq!(cmd0_after, cmd0_before);
    assert_eq!(cmd1_after, cmd1_before);
}
