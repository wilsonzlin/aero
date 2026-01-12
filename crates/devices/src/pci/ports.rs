use super::{PciBus, PciBusSnapshot, PciConfigMechanism1, PciPlatform};
use aero_io_snapshot::io::state::{
    IoSnapshot, SnapshotReader, SnapshotResult, SnapshotVersion, SnapshotWriter,
};
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
        Self {
            cfg: PciConfigMechanism1::new(),
            bus,
        }
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

    /// Reset the PCI configuration mechanism back to its power-on I/O state.
    ///
    /// This clears the `0xCF8` address latch (Configuration Mechanism #1) but preserves the PCI
    /// bus topology. Platform reset flows should separately re-run firmware/BIOS initialization
    /// (e.g. BAR assignment) if required.
    pub fn reset_io_state(&mut self) {
        self.cfg = PciConfigMechanism1::new();
    }
}

impl Default for PciConfigPorts {
    fn default() -> Self {
        Self::new()
    }
}

impl IoSnapshot for PciConfigPorts {
    const DEVICE_ID: [u8; 4] = *b"PCPT";
    const DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(1, 0);

    fn save_state(&self) -> Vec<u8> {
        const TAG_CFG: u16 = 1;
        const TAG_BUS: u16 = 2;

        let mut w = SnapshotWriter::new(Self::DEVICE_ID, Self::DEVICE_VERSION);
        w.field_bytes(TAG_CFG, self.cfg.save_state());

        let bus_snapshot = PciBusSnapshot::save_from(&self.bus);
        w.field_bytes(TAG_BUS, bus_snapshot.save_state());

        w.finish()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        const TAG_CFG: u16 = 1;
        const TAG_BUS: u16 = 2;

        let r = SnapshotReader::parse(bytes, Self::DEVICE_ID)?;
        r.ensure_device_major(Self::DEVICE_VERSION.major)?;

        if let Some(buf) = r.bytes(TAG_CFG) {
            self.cfg.load_state(buf)?;
        }

        if let Some(buf) = r.bytes(TAG_BUS) {
            let mut snapshot = PciBusSnapshot::default();
            snapshot.load_state(buf)?;
            snapshot.restore_into(&mut self.bus)?;
        }

        Ok(())
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

    fn reset(&mut self) {
        // Reset the shared config-mechanism state back to its power-on value. This is safe to call
        // multiple times (once per port mapping) as the operation is idempotent.
        self.cfg.borrow_mut().reset_io_state();
    }
}

/// Range-mapped PCI config mechanism #1 ports.
///
/// The bus mapping spans `0xCF8..=0xCFF` so byte/word accesses to both the address and data dwords
/// are handled correctly. Individual ports (e.g. `0xCF9`) may be overridden by exact port mappings
/// such as the legacy reset control register.
struct PciConfigPortRange {
    cfg: SharedPciConfigPorts,
}

impl PciConfigPortRange {
    pub fn new(cfg: SharedPciConfigPorts) -> Self {
        Self { cfg }
    }
}

impl PortIoDevice for PciConfigPortRange {
    fn read(&mut self, port: u16, size: u8) -> u32 {
        self.cfg.borrow_mut().io_read(port, size)
    }

    fn write(&mut self, port: u16, size: u8, value: u32) {
        self.cfg.borrow_mut().io_write(port, size, value);
    }

    fn reset(&mut self) {
        // Reset the shared config-mechanism state back to its power-on value. This is safe to call
        // multiple times and does not affect the PCI bus topology.
        self.cfg.borrow_mut().reset_io_state();
    }
}

pub fn register_pci_config_ports(bus: &mut IoPortBus, cfg: SharedPciConfigPorts) {
    const PCI_CFG_PORTS_LEN: u16 = (PCI_CFG_DATA_PORT + 4) - PCI_CFG_ADDR_PORT;
    bus.register_range(
        PCI_CFG_ADDR_PORT,
        PCI_CFG_PORTS_LEN,
        Box::new(PciConfigPortRange::new(cfg)),
    );
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
        cfg.set_bar_definition(
            0,
            PciBarDefinition::Mmio32 {
                size: 0x1000,
                prefetchable: false,
            },
        );
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
