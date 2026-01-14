//! Adapters for using the canonical `aero_devices::pci` implementation with the firmware BIOS
//! PCI enumeration code (`firmware::bios`).
//!
//! The firmware BIOS only needs PCI Configuration Mechanism #1 style 32-bit config-space accesses
//! (used during POST for enumeration and Interrupt Line programming). The canonical PCI model
//! (`aero_devices::pci`) already implements config-mech1 behind the standard `0xCF8/0xCFC` I/O port
//! pair, so the adapters here simply issue those port accesses directly.

#![forbid(unsafe_code)]

use aero_devices::pci::{
    PciBdf, PciBus, PciConfigPorts, SharedPciConfigPorts, PCI_CFG_ADDR_PORT, PCI_CFG_DATA_PORT,
};

/// Firmware BIOS [`firmware::bios::PciConfigSpace`] adapter for an owned
/// [`aero_devices::pci::PciConfigPorts`] instance.
pub struct PciConfigPortsBiosAdapter<'a> {
    ports: &'a mut PciConfigPorts,
}

impl<'a> PciConfigPortsBiosAdapter<'a> {
    pub fn new(ports: &'a mut PciConfigPorts) -> Self {
        Self { ports }
    }
}

/// Firmware BIOS [`firmware::bios::PciConfigSpace`] adapter for a shared
/// [`aero_devices::pci::SharedPciConfigPorts`].
///
/// This is the common representation used by platform code that needs to share access between the
/// guest's port-I/O bus and firmware.
#[derive(Clone)]
pub struct SharedPciConfigPortsBiosAdapter {
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
/// `0xCF8/0xCFC` config ports.
pub struct PciBusBiosAdapter<'a> {
    bus: &'a mut PciBus,
}

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
    use aero_devices::pci::{PciBdf, PciBus, PciConfigSpace, PciInterruptPin};
    use aero_pci_routing as pci_routing;
    use firmware::bios::{A20Gate, Bios, BiosConfig, FirmwareMemory, InMemoryDisk, BIOS_SECTOR_SIZE};
    use memory::{DenseMemory, MapError, PhysicalMemoryBus};
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::Arc;

    struct StubPciDev {
        cfg: PciConfigSpace,
    }

    impl aero_devices::pci::PciDevice for StubPciDev {
        fn config(&self) -> &PciConfigSpace {
            &self.cfg
        }

        fn config_mut(&mut self) -> &mut PciConfigSpace {
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

    fn boot_sector() -> [u8; BIOS_SECTOR_SIZE] {
        let mut sector = [0u8; BIOS_SECTOR_SIZE];
        sector[510] = 0x55;
        sector[511] = 0xAA;
        sector
    }

    fn expected_irq_line(pirq_to_gsi: [u32; 4], device: u8, interrupt_pin: u8) -> u8 {
        pci_routing::irq_line_for_intx(pirq_to_gsi, device, interrupt_pin)
    }

    fn pci_bus_with_single_dev(bdf: PciBdf, interrupt_pin: PciInterruptPin) -> PciBus {
        let mut pci_bus = PciBus::new();
        let mut cfg = PciConfigSpace::new(0x1234, 0x5678);
        cfg.set_interrupt_pin(interrupt_pin.to_config_u8());
        pci_bus.add_device(bdf, Box::new(StubPciDev { cfg }));
        pci_bus
    }

    #[test]
    fn pci_firmware_adapter_masks_misaligned_config_dword_offsets() {
        let bdf = PciBdf::new(0, 1, 0);
        let pci_ports: SharedPciConfigPorts = Rc::new(RefCell::new(PciConfigPorts::with_bus(
            pci_bus_with_single_dev(bdf, PciInterruptPin::IntA),
        )));
        let mut adapter = SharedPciConfigPortsBiosAdapter::new(pci_ports.clone());

        let before = firmware::bios::PciConfigSpace::read_config_dword(&mut adapter, 0, 1, 0, 0x3C);
        let before_misaligned =
            firmware::bios::PciConfigSpace::read_config_dword(&mut adapter, 0, 1, 0, 0x3D);
        assert_eq!(before, before_misaligned);

        let new_line = 0x5A;
        let new = (before & 0xFFFF_FF00) | u32::from(new_line);
        // The adapter should treat a misaligned offset like config-mech1 hardware: mask with
        // `offset & 0xFC` (i.e. write the aligned 0x3C dword).
        firmware::bios::PciConfigSpace::write_config_dword(&mut adapter, 0, 1, 0, 0x3D, new);

        let after = firmware::bios::PciConfigSpace::read_config_dword(&mut adapter, 0, 1, 0, 0x3C);
        let after_misaligned =
            firmware::bios::PciConfigSpace::read_config_dword(&mut adapter, 0, 1, 0, 0x3D);
        assert_eq!(after, new);
        assert_eq!(after_misaligned, new);

        let mut pci_ports = pci_ports.borrow_mut();
        let cfg = pci_ports.bus_mut().device_config_mut(bdf).unwrap();
        assert_eq!(cfg.interrupt_line(), new_line);
        assert_eq!(cfg.interrupt_pin(), PciInterruptPin::IntA.to_config_u8());
    }

    #[test]
    fn bios_enumeration_programs_interrupt_line_via_cfg_ports_adapter() {
        let bdf = PciBdf::new(0, 1, 0);
        let mut pci_ports =
            PciConfigPorts::with_bus(pci_bus_with_single_dev(bdf, PciInterruptPin::IntA));
        let mut adapter = PciConfigPortsBiosAdapter::new(&mut pci_ports);

        let mut mem = TestMemory::new(16 * 1024 * 1024);
        let mut cpu = aero_cpu_core::state::CpuState::new(aero_cpu_core::state::CpuMode::Real);
        let mut disk = InMemoryDisk::from_boot_sector(boot_sector());

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

        bios.post_with_pci(&mut cpu, &mut mem, &mut disk, None, Some(&mut adapter));

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
    fn bios_enumeration_programs_interrupt_line_via_pci_bus_adapter() {
        let bdf = PciBdf::new(0, 1, 0);
        let mut pci_bus = pci_bus_with_single_dev(bdf, PciInterruptPin::IntA);
        let mut adapter = PciBusBiosAdapter::new(&mut pci_bus);

        let mut mem = TestMemory::new(16 * 1024 * 1024);
        let mut cpu = aero_cpu_core::state::CpuState::new(aero_cpu_core::state::CpuMode::Real);
        let mut disk = InMemoryDisk::from_boot_sector(boot_sector());

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

        bios.post_with_pci(&mut cpu, &mut mem, &mut disk, None, Some(&mut adapter));

        let line = pci_bus.device_config_mut(bdf).unwrap().interrupt_line();
        assert_eq!(line, expected);
        let pin = pci_bus.device_config_mut(bdf).unwrap().interrupt_pin();
        assert_eq!(pin, PciInterruptPin::IntA.to_config_u8());
    }

    #[test]
    fn bios_enumeration_programs_interrupt_line_via_shared_cfg_ports_adapter() {
        let bdf = PciBdf::new(0, 1, 0);
        let pci_ports: SharedPciConfigPorts = Rc::new(RefCell::new(PciConfigPorts::with_bus(
            pci_bus_with_single_dev(bdf, PciInterruptPin::IntA),
        )));
        let mut adapter = SharedPciConfigPortsBiosAdapter::new(pci_ports.clone());

        let mut mem = TestMemory::new(16 * 1024 * 1024);
        let mut cpu = aero_cpu_core::state::CpuState::new(aero_cpu_core::state::CpuMode::Real);
        let mut disk = InMemoryDisk::from_boot_sector(boot_sector());

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

        bios.post_with_pci(&mut cpu, &mut mem, &mut disk, None, Some(&mut adapter));

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
