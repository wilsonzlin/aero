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
        if bus.device_config(bdf).is_some() { 0 } else { 0xFF }
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

        let reg_usize = usize::from(reg);
        if (0x10..=0x27).contains(&reg_usize) {
            // BAR registers require 32-bit aligned writes in [`PciConfigSpace`]. Emulate subword
            // updates by read-modify-writing the containing DWORD.
            let aligned = reg & !0x3;
            let shift = u32::from(reg - aligned) * 8;
            let mut dword = bus.read_config(bdf, aligned, 4);
            dword = (dword & !(0xFFu32 << shift)) | (u32::from(value) << shift);
            bus.write_config(bdf, aligned, 4, dword);
            return;
        }

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
            let byte = self.read_u8(offset.wrapping_add(i as u64));
            value |= (byte as u64) << (8 * i);
        }
        value
    }

    fn write(&mut self, offset: u64, size: usize, value: u64) {
        if size == 0 || size > 8 {
            return;
        }

        let bytes = value.to_le_bytes();
        for i in 0..size {
            self.write_u8(offset.wrapping_add(i as u64), bytes[i]);
        }
    }
}

