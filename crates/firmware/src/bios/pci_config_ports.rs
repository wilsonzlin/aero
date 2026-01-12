use crate::bios::PciConfigSpace;
use aero_devices::pci::{PciConfigPorts, SharedPciConfigPorts, PCI_CFG_ADDR_PORT, PCI_CFG_DATA_PORT};

/// BIOS [`PciConfigSpace`] adapter that talks to a PCI bus through
/// config-mechanism-1 IO ports (0xCF8/0xCFC).
///
/// This forwards to [`aero_devices::pci::PciConfigPorts`], which already contains a full
/// config-mech-1 implementation (address latch + data window access sizes).
pub struct PciConfigPortsAdapter<P> {
    ports: P,
}

impl<'a> PciConfigPortsAdapter<&'a mut PciConfigPorts> {
    pub fn new(ports: &'a mut PciConfigPorts) -> Self {
        Self { ports }
    }
}

impl PciConfigPortsAdapter<SharedPciConfigPorts> {
    pub fn new_shared(ports: SharedPciConfigPorts) -> Self {
        Self { ports }
    }
}

impl<P> PciConfigPortsAdapter<P> {
    fn cfg_addr(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
        assert_eq!(
            offset & 0x3,
            0,
            "PCI config dword offset must be 4-byte aligned"
        );
        debug_assert!(device < 32);
        debug_assert!(function < 8);

        0x8000_0000
            | ((bus as u32) << 16)
            | ((device as u32) << 11)
            | ((function as u32) << 8)
            | ((offset as u32) & 0xFC)
    }
}

impl<'a> PciConfigSpace for PciConfigPortsAdapter<&'a mut PciConfigPorts> {
    fn read_config_dword(&mut self, bus: u8, device: u8, function: u8, offset: u8) -> u32 {
        let addr = Self::cfg_addr(bus, device, function, offset);
        self.ports.io_write(PCI_CFG_ADDR_PORT, 4, addr);
        self.ports.io_read(PCI_CFG_DATA_PORT, 4)
    }

    fn write_config_dword(&mut self, bus: u8, device: u8, function: u8, offset: u8, value: u32) {
        let addr = Self::cfg_addr(bus, device, function, offset);
        self.ports.io_write(PCI_CFG_ADDR_PORT, 4, addr);
        self.ports.io_write(PCI_CFG_DATA_PORT, 4, value);
    }
}

impl PciConfigSpace for PciConfigPortsAdapter<SharedPciConfigPorts> {
    fn read_config_dword(&mut self, bus: u8, device: u8, function: u8, offset: u8) -> u32 {
        let addr = Self::cfg_addr(bus, device, function, offset);
        let mut ports = self.ports.borrow_mut();
        ports.io_write(PCI_CFG_ADDR_PORT, 4, addr);
        ports.io_read(PCI_CFG_DATA_PORT, 4)
    }

    fn write_config_dword(&mut self, bus: u8, device: u8, function: u8, offset: u8, value: u32) {
        let addr = Self::cfg_addr(bus, device, function, offset);
        let mut ports = self.ports.borrow_mut();
        ports.io_write(PCI_CFG_ADDR_PORT, 4, addr);
        ports.io_write(PCI_CFG_DATA_PORT, 4, value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bios::{Bios, BiosConfig, InMemoryDisk, TestMemory};
    use aero_cpu_core::state::{CpuMode, CpuState};
    use aero_devices::pci::{profile, PciBdf, PciDevice};

    struct ProfiledDevice {
        cfg: aero_devices::pci::PciConfigSpace,
    }

    impl ProfiledDevice {
        fn new(profile: profile::PciDeviceProfile) -> Self {
            Self {
                cfg: profile.build_config_space(),
            }
        }
    }

    impl PciDevice for ProfiledDevice {
        fn config(&self) -> &aero_devices::pci::PciConfigSpace {
            &self.cfg
        }

        fn config_mut(&mut self) -> &mut aero_devices::pci::PciConfigSpace {
            &mut self.cfg
        }
    }

    #[test]
    fn post_programs_interrupt_line_through_config_ports_adapter() {
        let mut mem = TestMemory::new(16 * 1024 * 1024);
        let mut cpu = CpuState::new(CpuMode::Real);

        let mut sector = [0u8; 512];
        sector[510] = 0x55;
        sector[511] = 0xAA;
        let mut disk = InMemoryDisk::from_boot_sector(sector);

        let mut cfg_ports = PciConfigPorts::new();

        // Place a profiled E1000 device at 00:05.0 with INTA#.
        let bdf = PciBdf::new(0, 5, 0);
        cfg_ports
            .bus_mut()
            .add_device(bdf, Box::new(ProfiledDevice::new(profile::NIC_E1000_82540EM)));

        // The profiled device config-space is pre-seeded with the default routing (typically 11 for
        // slot 5 INTA#). Override the BIOS routing table so we can assert the BIOS actually wrote
        // the Interrupt Line register through the adapter.
        let custom_pirq_to_gsi = [32, 33, 34, 35];
        let mut bios = Bios::new(BiosConfig {
            enable_acpi: false,
            pirq_to_gsi: custom_pirq_to_gsi,
            ..BiosConfig::default()
        });

        let mut adapter = PciConfigPortsAdapter::new(&mut cfg_ports);
        bios.post_with_pci(&mut cpu, &mut mem, &mut disk, Some(&mut adapter));

        // Slot 5, INTA# => PIRQ index = (dev + pin) mod 4 = (5 + 0) mod 4 = 1.
        let expected_line = 33u8;

        let cfg = cfg_ports.bus_mut().device_config_mut(bdf).unwrap();
        assert_eq!(cfg.interrupt_pin(), 1);
        assert_eq!(cfg.interrupt_line(), expected_line);

        // BIOS bookkeeping should also reflect the chosen routing.
        let seen = bios
            .pci_devices()
            .iter()
            .find(|d| d.bus == 0 && d.device == 5 && d.function == 0)
            .expect("BIOS did not enumerate the profiled device");
        assert_eq!(seen.irq_line, expected_line);
    }
}
