use crate::pci::{PciBdf, SharedPciConfigPorts};

/// Number of bytes covered by one bus worth of PCIe ECAM configuration space.
///
/// The ECAM layout is:
/// - 256 buses
/// - 32 devices per bus
/// - 8 functions per device
/// - 4KiB config space per function
///
/// Which yields 32 * 8 * 4096 = 1MiB per bus.
pub const PCIE_ECAM_BUS_STRIDE: u64 = 1 << 20;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct PciEcamConfig {
    pub segment: u16,
    pub start_bus: u8,
    pub end_bus: u8,
}

impl PciEcamConfig {
    pub fn window_size_bytes(self) -> u64 {
        let start = u64::from(self.start_bus);
        let end = u64::from(self.end_bus);
        if end < start {
            return 0;
        }
        (end - start + 1) * PCIE_ECAM_BUS_STRIDE
    }
}

/// [`memory::MmioHandler`] exposing PCI configuration space through PCIe ECAM ("MMCONFIG").
///
/// This handler shares the same backing [`crate::pci::PciBus`] used by the legacy PCI config
/// mechanism #1 port interface (0xCF8/0xCFC), ensuring both paths remain coherent.
pub struct PciEcamMmio {
    cfg_ports: SharedPciConfigPorts,
    cfg: PciEcamConfig,
}

impl PciEcamMmio {
    pub fn new(cfg_ports: SharedPciConfigPorts, cfg: PciEcamConfig) -> Self {
        Self { cfg_ports, cfg }
    }

    fn decode(&self, offset: u64) -> Option<(PciBdf, u16)> {
        let bus_index = offset >> 20;
        let device = ((offset >> 15) & 0x1f) as u8;
        let function = ((offset >> 12) & 0x07) as u8;
        let reg = (offset & 0x0fff) as u16;

        let start = u64::from(self.cfg.start_bus);
        let end = u64::from(self.cfg.end_bus);
        let bus = start.checked_add(bus_index)?;
        if bus > end {
            return None;
        }

        let bus = u8::try_from(bus).ok()?;
        Some((PciBdf::new(bus, device, function), reg))
    }

    fn read_u8(&mut self, offset: u64) -> u8 {
        let Some((bdf, reg)) = self.decode(offset) else {
            return 0xFF;
        };

        let mut ports = self.cfg_ports.borrow_mut();
        let bus = ports.bus_mut();

        if reg < 0x100 {
            return bus.read_config(bdf, reg, 1) as u8;
        }

        // Our PCI model only implements the first 256 bytes of config space. For present devices,
        // treat the rest of the 4KiB ECAM function window as zero-filled; absent devices continue
        // to float high.
        if bus.device_config(bdf).is_some() {
            0
        } else {
            0xFF
        }
    }

    fn write_u8(&mut self, offset: u64, value: u8) {
        let Some((bdf, reg)) = self.decode(offset) else {
            return;
        };

        if reg >= 0x100 {
            return;
        }

        let mut ports = self.cfg_ports.borrow_mut();
        let bus = ports.bus_mut();

        // `PciBus::write_config` emulates subword BAR writes via a read-modify-write of the
        // containing DWORD, so we can forward byte writes directly.
        bus.write_config(bdf, reg, 1, u32::from(value));
    }
}

impl memory::MmioHandler for PciEcamMmio {
    fn read(&mut self, offset: u64, size: usize) -> u64 {
        if size == 0 || size > 8 {
            return 0;
        }

        let mut value = 0u64;
        for i in 0..size {
            let byte = match offset.checked_add(i as u64) {
                Some(off) => self.read_u8(off),
                None => 0xFF,
            };
            value |= (byte as u64) << (8 * i);
        }
        value
    }

    fn write(&mut self, offset: u64, size: usize, value: u64) {
        if size == 0 || size > 8 {
            return;
        }

        let bytes = value.to_le_bytes();
        for (i, byte) in bytes.iter().copied().enumerate().take(size) {
            let Some(off) = offset.checked_add(i as u64) else {
                continue;
            };
            self.write_u8(off, byte);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pci::{PciBus, PciConfigPorts, PciConfigSpace, PciDevice};
    use memory::MmioHandler;
    use std::cell::RefCell;
    use std::rc::Rc;

    struct StubDevice {
        cfg: PciConfigSpace,
    }

    impl StubDevice {
        fn new(vendor_id: u16, device_id: u16) -> Self {
            Self {
                cfg: PciConfigSpace::new(vendor_id, device_id),
            }
        }
    }

    impl PciDevice for StubDevice {
        fn config(&self) -> &PciConfigSpace {
            &self.cfg
        }

        fn config_mut(&mut self) -> &mut PciConfigSpace {
            &mut self.cfg
        }
    }

    #[test]
    fn ecam_wide_access_near_u64_max_does_not_wrap_offsets() {
        // Ensure that wide reads/writes near `u64::MAX` do not wrap around and access low ECAM
        // offsets.
        let mut bus = PciBus::new();
        bus.add_device(
            PciBdf::new(0, 0, 0),
            Box::new(StubDevice::new(0x1234, 0x5678)),
        );

        let cfg_ports: SharedPciConfigPorts = Rc::new(RefCell::new(PciConfigPorts::with_bus(bus)));
        let cfg = PciEcamConfig {
            segment: 0,
            start_bus: 0,
            end_bus: 0,
        };
        let mut ecam = PciEcamMmio::new(Rc::clone(&cfg_ports), cfg);

        // Old behavior would wrap the final byte to offset 1 (vendor ID high byte = 0x12).
        let got = ecam.read(u64::MAX - 2, 4);
        assert_eq!(got, 0xFFFF_FFFF);

        // Old behavior would wrap the final byte write to offset 4 (command low byte), mutating
        // config space. Hardened behavior must ignore the out-of-range bytes.
        assert_eq!(ecam.read(0x04, 2), 0);
        ecam.write(u64::MAX - 2, 8, 0x02u64 << 56);
        assert_eq!(ecam.read(0x04, 2), 0);
    }
}
