use super::{PciBus, PciConfigMechanism1, PciPlatform};
use aero_platform::io::{IoPortBus, PortIoDevice};
use std::cell::RefCell;
use std::rc::Rc;

pub const PCI_CFG_ADDR_PORT: u16 = 0xCF8;
pub const PCI_CFG_DATA_PORT: u16 = 0xCFC;

pub struct PciConfigPorts {
    cfg: PciConfigMechanism1,
    bus: PciBus,
}

impl PciConfigPorts {
    pub fn new() -> Self {
        Self::with_bus(PciPlatform::build_bus())
    }

    pub fn with_bus(bus: PciBus) -> Self {
        Self { cfg: PciConfigMechanism1::new(), bus }
    }

    pub fn bus_mut(&mut self) -> &mut PciBus {
        &mut self.bus
    }

    pub fn io_read(&mut self, port: u16, size: u8) -> u32 {
        self.cfg.io_read(&mut self.bus, port, size)
    }

    pub fn io_write(&mut self, port: u16, size: u8, value: u32) {
        self.cfg.io_write(&mut self.bus, port, size, value);
    }
}

impl Default for PciConfigPorts {
    fn default() -> Self {
        Self::new()
    }
}

pub type SharedPciConfigPorts = Rc<RefCell<PciConfigPorts>>;

pub struct PciConfigPort {
    cfg: SharedPciConfigPorts,
    port: u16,
}

impl PciConfigPort {
    pub fn new(cfg: SharedPciConfigPorts, port: u16) -> Self {
        Self { cfg, port }
    }
}

impl PortIoDevice for PciConfigPort {
    fn read(&mut self, port: u16, size: u8) -> u32 {
        debug_assert_eq!(port, self.port);
        self.cfg.borrow_mut().io_read(port, size)
    }

    fn write(&mut self, port: u16, size: u8, value: u32) {
        debug_assert_eq!(port, self.port);
        self.cfg.borrow_mut().io_write(port, size, value);
    }
}

pub fn register_pci_config_ports(bus: &mut IoPortBus, cfg: SharedPciConfigPorts) {
    for port in PCI_CFG_ADDR_PORT..=PCI_CFG_DATA_PORT + 3 {
        bus.register(port, Box::new(PciConfigPort::new(cfg.clone(), port)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pci::{PciBarDefinition, PciBdf, PciConfigSpace, PciDevice};

    #[test]
    fn cfg_ports_read_write_and_bar_probe() {
        let mut cfg_ports = PciConfigPorts::new();

        struct TestDev {
            cfg: PciConfigSpace,
        }

        impl PciDevice for TestDev {
            fn config(&self) -> &PciConfigSpace {
                &self.cfg
            }

            fn config_mut(&mut self) -> &mut PciConfigSpace {
                &mut self.cfg
            }
        }

        let mut cfg = PciConfigSpace::new(0x1234, 0x5678);
        cfg.set_bar_definition(0, PciBarDefinition::Mmio32 { size: 0x1000, prefetchable: false });
        cfg.set_bar_definition(1, PciBarDefinition::Io { size: 0x20 });

        cfg_ports
            .bus_mut()
            .add_device(PciBdf::new(0, 1, 0), Box::new(TestDev { cfg }));

        // Read vendor/device ID.
        cfg_ports.io_write(0xCF8, 4, 0x8000_0000 | (1 << 11));
        let id = cfg_ports.io_read(0xCFC, 4);
        assert_eq!(id & 0xFFFF, 0x1234);
        assert_eq!(id >> 16, 0x5678);

        // BAR0 probe.
        cfg_ports.io_write(0xCF8, 4, 0x8000_0000 | (1 << 11) | 0x10);
        cfg_ports.io_write(0xCFC, 4, 0xFFFF_FFFF);
        let bar0_mask = cfg_ports.io_read(0xCFC, 4);
        assert_eq!(bar0_mask, 0xFFFF_F000);

        // Program BAR0 address.
        cfg_ports.io_write(0xCFC, 4, 0x8000_0000);
        assert_eq!(cfg_ports.io_read(0xCFC, 4), 0x8000_0000);

        // BAR1 IO probe.
        cfg_ports.io_write(0xCF8, 4, 0x8000_0000 | (1 << 11) | 0x14);
        cfg_ports.io_write(0xCFC, 4, 0xFFFF_FFFF);
        let bar1_mask = cfg_ports.io_read(0xCFC, 4);
        assert_eq!(bar1_mask, 0xFFFF_FFE1);
    }
}
