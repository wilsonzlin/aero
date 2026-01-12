//! Adapters for using the canonical `aero_devices::pci` implementation with the firmware BIOS
//! PCI enumeration code (`firmware::bios`).
//!
//! The firmware BIOS code only needs PCI Configuration Mechanism #1 style 32-bit accesses to
//! enumerate devices and program the Interrupt Line register (0x3C). The canonical PCI model
//! (`aero_devices::pci::PciConfigPorts`) already implements config-mech1 behind the standard
//! 0xCF8/0xCFC port pair, so the adapter just issues those port accesses directly.

use aero_devices::pci::{
    PciBdf, PciBus, PciConfigPorts, SharedPciConfigPorts, PCI_CFG_ADDR_PORT, PCI_CFG_DATA_PORT,
};

/// Firmware BIOS [`firmware::bios::PciConfigSpace`] adapter for an owned [`PciConfigPorts`]
/// instance.
#[allow(dead_code)]
pub(crate) struct PciConfigPortsBiosAdapter<'a> {
    ports: &'a mut PciConfigPorts,
}

#[allow(dead_code)]
impl<'a> PciConfigPortsBiosAdapter<'a> {
    pub fn new(ports: &'a mut PciConfigPorts) -> Self {
        Self { ports }
    }
}

/// Firmware BIOS [`firmware::bios::PciConfigSpace`] adapter for a shared
/// [`SharedPciConfigPorts`].
///
/// This is the common representation used by platform code that needs to share access between the
/// guest's port I/O bus and firmware.
#[derive(Clone)]
pub(crate) struct SharedPciConfigPortsBiosAdapter {
    ports: SharedPciConfigPorts,
}

impl SharedPciConfigPortsBiosAdapter {
    pub fn new(ports: SharedPciConfigPorts) -> Self {
        Self { ports }
    }
}

/// Firmware BIOS [`firmware::bios::PciConfigSpace`] adapter for direct access to a [`PciBus`].
///
/// This is useful when a caller already has a configured PCI bus but is not modelling the legacy
/// 0xCF8/0xCFC config ports.
#[allow(dead_code)]
pub(crate) struct PciBusBiosAdapter<'a> {
    bus: &'a mut PciBus,
}

#[allow(dead_code)]
impl<'a> PciBusBiosAdapter<'a> {
    pub fn new(bus: &'a mut PciBus) -> Self {
        Self { bus }
    }
}

fn cfg_addr(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    // PCI Configuration Mechanism #1 address (0xCF8).
    //
    // Bits 7:2 encode the DWORD-aligned register number; bits 1:0 are reserved and read as 0.
    0x8000_0000
        | (u32::from(bus) << 16)
        | (u32::from(device) << 11)
        | (u32::from(function) << 8)
        | (u32::from(offset) & 0xFC)
}

fn read_config_dword_via_ports(
    ports: &mut PciConfigPorts,
    bus: u8,
    device: u8,
    function: u8,
    offset: u8,
) -> u32 {
    ports.io_write(
        PCI_CFG_ADDR_PORT,
        4,
        cfg_addr(bus, device, function, offset),
    );
    ports.io_read(PCI_CFG_DATA_PORT, 4)
}

fn write_config_dword_via_ports(
    ports: &mut PciConfigPorts,
    bus: u8,
    device: u8,
    function: u8,
    offset: u8,
    value: u32,
) {
    ports.io_write(
        PCI_CFG_ADDR_PORT,
        4,
        cfg_addr(bus, device, function, offset),
    );
    ports.io_write(PCI_CFG_DATA_PORT, 4, value);
}

impl firmware::bios::PciConfigSpace for PciConfigPortsBiosAdapter<'_> {
    fn read_config_dword(&mut self, bus: u8, device: u8, function: u8, offset: u8) -> u32 {
        read_config_dword_via_ports(self.ports, bus, device, function, offset)
    }

    fn write_config_dword(&mut self, bus: u8, device: u8, function: u8, offset: u8, value: u32) {
        write_config_dword_via_ports(self.ports, bus, device, function, offset, value);
    }
}

impl firmware::bios::PciConfigSpace for SharedPciConfigPortsBiosAdapter {
    fn read_config_dword(&mut self, bus: u8, device: u8, function: u8, offset: u8) -> u32 {
        let mut ports = self.ports.borrow_mut();
        read_config_dword_via_ports(&mut ports, bus, device, function, offset)
    }

    fn write_config_dword(&mut self, bus: u8, device: u8, function: u8, offset: u8, value: u32) {
        let mut ports = self.ports.borrow_mut();
        write_config_dword_via_ports(&mut ports, bus, device, function, offset, value);
    }
}

impl firmware::bios::PciConfigSpace for PciBusBiosAdapter<'_> {
    fn read_config_dword(&mut self, bus: u8, device: u8, function: u8, offset: u8) -> u32 {
        let bdf = PciBdf::new(bus, device, function);
        let offset = u16::from(offset & 0xFC);
        self.bus.read_config(bdf, offset, 4)
    }

    fn write_config_dword(&mut self, bus: u8, device: u8, function: u8, offset: u8, value: u32) {
        let bdf = PciBdf::new(bus, device, function);
        let offset = u16::from(offset & 0xFC);
        self.bus.write_config(bdf, offset, 4, value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aero_devices::pci::{PciBdf, PciBus, PciInterruptPin};
    use firmware::bios::{A20Gate, Bios, BiosConfig, BlockDevice, FirmwareMemory, InMemoryDisk};
    use memory::{DenseMemory, MapError, PhysicalMemoryBus};
    use pretty_assertions::assert_eq;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::Arc;

    struct StubPciDev {
        cfg: aero_devices::pci::PciConfigSpace,
    }

    impl aero_devices::pci::PciDevice for StubPciDev {
        fn config(&self) -> &aero_devices::pci::PciConfigSpace {
            &self.cfg
        }

        fn config_mut(&mut self) -> &mut aero_devices::pci::PciConfigSpace {
            &mut self.cfg
        }
    }

    struct TestMemory {
        a20_enabled: bool,
        inner: PhysicalMemoryBus,
    }

    impl TestMemory {
        fn new(size: u64) -> Self {
            let ram = DenseMemory::new(size).expect("guest RAM allocation failed");
            Self {
                a20_enabled: false,
                inner: PhysicalMemoryBus::new(Box::new(ram)),
            }
        }

        fn translate_a20(&self, addr: u64) -> u64 {
            if self.a20_enabled {
                addr
            } else {
                addr & !(1u64 << 20)
            }
        }
    }

    impl A20Gate for TestMemory {
        fn set_a20_enabled(&mut self, enabled: bool) {
            self.a20_enabled = enabled;
        }

        fn a20_enabled(&self) -> bool {
            self.a20_enabled
        }
    }

    impl FirmwareMemory for TestMemory {
        fn map_rom(&mut self, base: u64, rom: Arc<[u8]>) {
            let len = rom.len();
            match self.inner.map_rom(base, rom) {
                Ok(()) => {}
                Err(MapError::Overlap) => {
                    // BIOS resets may re-map the same ROM windows. Treat identical overlaps as
                    // idempotent, but reject unexpected overlaps to avoid silently corrupting the
                    // bus.
                    let already_mapped = self
                        .inner
                        .rom_regions()
                        .iter()
                        .any(|r| r.start == base && r.data.len() == len);
                    if !already_mapped {
                        panic!("unexpected ROM mapping overlap at 0x{base:016x}");
                    }
                }
                Err(MapError::AddressOverflow) => {
                    panic!("ROM mapping overflow at 0x{base:016x} (len=0x{len:x})")
                }
            }
        }
    }

    impl memory::MemoryBus for TestMemory {
        fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
            if self.a20_enabled {
                self.inner.read_physical(paddr, buf);
                return;
            }

            for (i, slot) in buf.iter_mut().enumerate() {
                let addr = self.translate_a20(paddr.wrapping_add(i as u64));
                *slot = self.inner.read_physical_u8(addr);
            }
        }

        fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
            if self.a20_enabled {
                self.inner.write_physical(paddr, buf);
                return;
            }

            for (i, byte) in buf.iter().copied().enumerate() {
                let addr = self.translate_a20(paddr.wrapping_add(i as u64));
                self.inner.write_physical_u8(addr, byte);
            }
        }
    }

    fn boot_sector() -> [u8; 512] {
        let mut sector = [0u8; 512];
        sector[510] = 0x55;
        sector[511] = 0xAA;
        sector
    }

    fn expected_irq_line(pirq_to_gsi: [u32; 4], device: u8, interrupt_pin: u8) -> u8 {
        // Must match the firmware BIOS swizzle in `firmware::bios::pci`:
        //   PIRQ = (device + (pin-1)) mod 4
        if interrupt_pin == 0 {
            return 0xFF;
        }
        let pin_index = interrupt_pin.wrapping_sub(1) & 0x03;
        let pirq = device.wrapping_add(pin_index) & 0x03;
        let gsi = pirq_to_gsi[pirq as usize];
        u8::try_from(gsi).unwrap_or(0xFF)
    }

    #[test]
    fn firmware_bios_pci_enumeration_programs_interrupt_line_via_cfg_ports() {
        let mut pci_bus = PciBus::new();

        let bdf = PciBdf::new(0, 1, 0);
        let mut cfg = aero_devices::pci::PciConfigSpace::new(0x1234, 0x5678);
        cfg.set_interrupt_pin(PciInterruptPin::IntA.to_config_u8());
        pci_bus.add_device(bdf, Box::new(StubPciDev { cfg }));

        let mut pci_ports = PciConfigPorts::with_bus(pci_bus);
        let mut adapter = PciConfigPortsBiosAdapter::new(&mut pci_ports);

        let mut mem = TestMemory::new(16 * 1024 * 1024);
        let mut cpu = aero_cpu_core::state::CpuState::new(aero_cpu_core::state::CpuMode::Real);
        let mut disk: Box<dyn BlockDevice> =
            Box::new(InMemoryDisk::from_boot_sector(boot_sector()));

        let bios_cfg = BiosConfig {
            enable_acpi: false,
            pirq_to_gsi: [40, 41, 42, 43],
            ..BiosConfig::default()
        };
        let expected = expected_irq_line(
            bios_cfg.pirq_to_gsi,
            bdf.device,
            PciInterruptPin::IntA.to_config_u8(),
        );
        let mut bios = Bios::new(bios_cfg);

        bios.post_with_pci(&mut cpu, &mut mem, &mut *disk, Some(&mut adapter));

        let line = pci_ports
            .bus_mut()
            .device_config_mut(bdf)
            .unwrap()
            .interrupt_line();
        assert_eq!(line, expected);
        let pin = pci_ports
            .bus_mut()
            .device_config_mut(bdf)
            .unwrap()
            .interrupt_pin();
        assert_eq!(pin, PciInterruptPin::IntA.to_config_u8());
    }

    #[test]
    fn firmware_bios_pci_enumeration_programs_interrupt_line_via_pci_bus() {
        let mut pci_bus = PciBus::new();

        let bdf = PciBdf::new(0, 1, 0);
        let mut cfg = aero_devices::pci::PciConfigSpace::new(0x1234, 0x5678);
        cfg.set_interrupt_pin(PciInterruptPin::IntA.to_config_u8());
        pci_bus.add_device(bdf, Box::new(StubPciDev { cfg }));

        let mut adapter = PciBusBiosAdapter::new(&mut pci_bus);

        let mut mem = TestMemory::new(16 * 1024 * 1024);
        let mut cpu = aero_cpu_core::state::CpuState::new(aero_cpu_core::state::CpuMode::Real);
        let mut disk: Box<dyn BlockDevice> =
            Box::new(InMemoryDisk::from_boot_sector(boot_sector()));

        let bios_cfg = BiosConfig {
            enable_acpi: false,
            pirq_to_gsi: [40, 41, 42, 43],
            ..BiosConfig::default()
        };
        let expected = expected_irq_line(
            bios_cfg.pirq_to_gsi,
            bdf.device,
            PciInterruptPin::IntA.to_config_u8(),
        );
        let mut bios = Bios::new(bios_cfg);

        bios.post_with_pci(&mut cpu, &mut mem, &mut *disk, Some(&mut adapter));

        let line = pci_bus.device_config_mut(bdf).unwrap().interrupt_line();
        assert_eq!(line, expected);
        let pin = pci_bus.device_config_mut(bdf).unwrap().interrupt_pin();
        assert_eq!(pin, PciInterruptPin::IntA.to_config_u8());
    }

    #[test]
    fn firmware_bios_pci_enumeration_programs_interrupt_line_via_shared_cfg_ports() {
        let mut pci_bus = PciBus::new();

        let bdf = PciBdf::new(0, 1, 0);
        let mut cfg = aero_devices::pci::PciConfigSpace::new(0x1234, 0x5678);
        cfg.set_interrupt_pin(PciInterruptPin::IntA.to_config_u8());
        pci_bus.add_device(bdf, Box::new(StubPciDev { cfg }));

        let pci_ports: SharedPciConfigPorts =
            Rc::new(RefCell::new(PciConfigPorts::with_bus(pci_bus)));
        let mut adapter = SharedPciConfigPortsBiosAdapter::new(pci_ports.clone());

        let mut mem = TestMemory::new(16 * 1024 * 1024);
        let mut cpu = aero_cpu_core::state::CpuState::new(aero_cpu_core::state::CpuMode::Real);
        let mut disk: Box<dyn BlockDevice> =
            Box::new(InMemoryDisk::from_boot_sector(boot_sector()));

        let bios_cfg = BiosConfig {
            enable_acpi: false,
            pirq_to_gsi: [40, 41, 42, 43],
            ..BiosConfig::default()
        };
        let expected = expected_irq_line(
            bios_cfg.pirq_to_gsi,
            bdf.device,
            PciInterruptPin::IntA.to_config_u8(),
        );
        let mut bios = Bios::new(bios_cfg);

        bios.post_with_pci(&mut cpu, &mut mem, &mut *disk, Some(&mut adapter));

        let line = pci_ports
            .borrow_mut()
            .bus_mut()
            .device_config_mut(bdf)
            .unwrap()
            .interrupt_line();
        assert_eq!(line, expected);
        let pin = pci_ports
            .borrow_mut()
            .bus_mut()
            .device_config_mut(bdf)
            .unwrap()
            .interrupt_pin();
        assert_eq!(pin, PciInterruptPin::IntA.to_config_u8());
    }
}
