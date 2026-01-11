pub mod aerogpu;
#[cfg(feature = "aerogpu-legacy")]
pub mod aerogpu_legacy;

use std::any::Any;
use std::collections::HashMap;

pub const CONFIG_ADDRESS_PORT: u16 = 0xCF8;
pub const CONFIG_DATA_PORT: u16 = 0xCFC;
pub const CONFIG_DATA_PORT_END: u16 = CONFIG_DATA_PORT + 3;

pub const CONFIG_SPACE_SIZE: u16 = 256;

pub trait PciFunction: Send {
    fn config_read(&mut self, offset: u16, size: u8) -> u32;
    fn config_write(&mut self, offset: u16, size: u8, value: u32);
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PciBarKind {
    Unused,
    Memory32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PciBar {
    kind: PciBarKind,
    base: u32,
    size: u32,
    prefetchable: bool,
    probe: bool,
}

impl Default for PciBar {
    fn default() -> Self {
        Self {
            kind: PciBarKind::Unused,
            base: 0,
            size: 0,
            prefetchable: false,
            probe: false,
        }
    }
}

impl PciBar {
    pub fn memory32(base: u32, size: u32, prefetchable: bool) -> Self {
        assert!(size.is_power_of_two(), "BAR size must be power-of-two");
        assert!(size >= 0x10, "BAR size must be at least 16 bytes");
        Self {
            kind: PciBarKind::Memory32,
            base: base & Self::mem32_addr_mask(size),
            size,
            prefetchable,
            probe: false,
        }
    }

    pub fn kind(&self) -> PciBarKind {
        self.kind
    }

    pub fn base(&self) -> u32 {
        self.base
    }

    pub fn size(&self) -> u32 {
        self.size
    }

    fn mem32_flags(&self) -> u32 {
        let mut flags = 0u32;
        // bit0 = 0 for memory space
        // bits1-2 = 0b00 for 32-bit BAR
        if self.prefetchable {
            flags |= 1 << 3;
        }
        flags
    }

    fn mem32_addr_mask(size: u32) -> u32 {
        // Memory BARs store address bits in 31..4.
        (!(size - 1)) & 0xFFFF_FFF0
    }

    pub fn read_raw(&self) -> u32 {
        match self.kind {
            PciBarKind::Unused => 0,
            PciBarKind::Memory32 => {
                if self.probe {
                    Self::mem32_addr_mask(self.size) | self.mem32_flags()
                } else {
                    (self.base & Self::mem32_addr_mask(self.size)) | self.mem32_flags()
                }
            }
        }
    }

    pub fn write_raw(&mut self, value: u32) {
        match self.kind {
            PciBarKind::Unused => {}
            PciBarKind::Memory32 => {
                if value == 0xFFFF_FFFF {
                    self.probe = true;
                    return;
                }
                self.probe = false;
                self.base = value & Self::mem32_addr_mask(self.size);
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct PciConfigSpace {
    vendor_id: u16,
    device_id: u16,
    command: u16,
    status: u16,
    revision_id: u8,
    prog_if: u8,
    subclass: u8,
    class_code: u8,
    header_type: u8,
    interrupt_line: u8,
    interrupt_pin: u8,
    pub bars: [PciBar; 6],
}

impl PciConfigSpace {
    pub fn new(vendor_id: u16, device_id: u16, class_code: u8, subclass: u8, prog_if: u8) -> Self {
        Self {
            vendor_id,
            device_id,
            command: 0,
            status: 0,
            revision_id: 0,
            prog_if,
            subclass,
            class_code,
            header_type: 0,
            interrupt_line: 0,
            interrupt_pin: 1,
            bars: [PciBar::default(); 6],
        }
    }

    pub fn set_header_type(&mut self, header_type: u8) {
        self.header_type = header_type;
    }

    pub fn read(&mut self, offset: u16, size: u8) -> u32 {
        assert!(offset < CONFIG_SPACE_SIZE, "PCI config offset out of range");
        assert!(matches!(size, 1 | 2 | 4), "invalid PCI config read size");
        let aligned = offset & !3;
        let shift = (offset - aligned) * 8;
        let dword = self.read_u32(aligned);
        match size {
            1 => (dword >> shift) & 0xFF,
            2 => (dword >> shift) & 0xFFFF,
            4 => dword,
            _ => unreachable!(),
        }
    }

    pub fn write(&mut self, offset: u16, size: u8, value: u32) {
        assert!(offset < CONFIG_SPACE_SIZE, "PCI config offset out of range");
        assert!(matches!(size, 1 | 2 | 4), "invalid PCI config write size");
        let aligned = offset & !3;
        let shift = (offset - aligned) * 8;
        let mut dword = self.read_u32(aligned);
        match size {
            1 => {
                dword = (dword & !(0xFF << shift)) | ((value & 0xFF) << shift);
            }
            2 => {
                dword = (dword & !(0xFFFF << shift)) | ((value & 0xFFFF) << shift);
            }
            4 => dword = value,
            _ => unreachable!(),
        }
        self.write_u32(aligned, dword);
    }

    fn read_u32(&mut self, offset: u16) -> u32 {
        match offset {
            0x00 => (self.device_id as u32) << 16 | (self.vendor_id as u32),
            0x04 => (self.status as u32) << 16 | (self.command as u32),
            0x08 => {
                (self.class_code as u32) << 24
                    | (self.subclass as u32) << 16
                    | (self.prog_if as u32) << 8
                    | (self.revision_id as u32)
            }
            0x0C => (self.header_type as u32) << 16,
            0x10..=0x24 => {
                let bar_index = ((offset - 0x10) / 4) as usize;
                self.bars[bar_index].read_raw()
            }
            0x3C => (self.interrupt_pin as u32) << 8 | (self.interrupt_line as u32),
            _ => 0,
        }
    }

    fn write_u32(&mut self, offset: u16, value: u32) {
        match offset {
            0x04 => {
                self.command = (value & 0xFFFF) as u16;
            }
            0x10..=0x24 => {
                let bar_index = ((offset - 0x10) / 4) as usize;
                self.bars[bar_index].write_raw(value);
            }
            _ => {}
        }
    }
}

#[derive(Default)]
pub struct PciBus {
    config_address: u32,
    functions: HashMap<(u8, u8, u8), Box<dyn PciFunction>>,
}

impl PciBus {
    pub fn insert_function<F>(&mut self, bus: u8, device: u8, function: u8, f: F)
    where
        F: PciFunction + 'static,
    {
        self.functions.insert((bus, device, function), Box::new(f));
    }

    pub fn function_mut(
        &mut self,
        bus: u8,
        device: u8,
        function: u8,
    ) -> Option<&mut (dyn PciFunction + '_)> {
        self.functions
            .get_mut(&(bus, device, function))
            .map(|func| func.as_mut() as &mut dyn PciFunction)
    }

    pub fn function_mut_typed<T: 'static>(
        &mut self,
        bus: u8,
        device: u8,
        function: u8,
    ) -> Option<&mut T> {
        self.functions
            .get_mut(&(bus, device, function))
            .and_then(|func| func.as_any_mut().downcast_mut::<T>())
    }

    pub fn config_read(&mut self, bus: u8, device: u8, function: u8, offset: u16, size: u8) -> u32 {
        if let Some(func) = self.functions.get_mut(&(bus, device, function)) {
            func.config_read(offset, size)
        } else {
            match size {
                1 => 0xFF,
                2 => 0xFFFF,
                4 => 0xFFFF_FFFF,
                _ => 0,
            }
        }
    }

    pub fn config_write(
        &mut self,
        bus: u8,
        device: u8,
        function: u8,
        offset: u16,
        size: u8,
        value: u32,
    ) {
        if let Some(func) = self.functions.get_mut(&(bus, device, function)) {
            func.config_write(offset, size, value);
        }
    }

    pub fn io_read(&mut self, port: u16, size: u8) -> u32 {
        match port {
            CONFIG_ADDRESS_PORT => self.read_config_address(size),
            CONFIG_DATA_PORT..=CONFIG_DATA_PORT_END => self.read_config_data(port, size),
            _ => 0,
        }
    }

    pub fn io_write(&mut self, port: u16, size: u8, value: u32) {
        match port {
            CONFIG_ADDRESS_PORT => self.write_config_address(size, value),
            CONFIG_DATA_PORT..=CONFIG_DATA_PORT_END => self.write_config_data(port, size, value),
            _ => {}
        }
    }

    fn read_config_address(&self, size: u8) -> u32 {
        match size {
            4 => self.config_address,
            2 => self.config_address & 0xFFFF,
            1 => self.config_address & 0xFF,
            _ => 0,
        }
    }

    fn write_config_address(&mut self, size: u8, value: u32) {
        if size == 4 {
            self.config_address = value;
        }
    }

    fn read_config_data(&mut self, port: u16, size: u8) -> u32 {
        if (self.config_address & 0x8000_0000) == 0 {
            return match size {
                1 => 0xFF,
                2 => 0xFFFF,
                4 => 0xFFFF_FFFF,
                _ => 0,
            };
        }

        let bus = ((self.config_address >> 16) & 0xFF) as u8;
        let device = ((self.config_address >> 11) & 0x1F) as u8;
        let function = ((self.config_address >> 8) & 0x07) as u8;
        let register_base = (self.config_address & 0xFC) as u16;
        let port_offset = port - CONFIG_DATA_PORT;
        let offset = register_base + port_offset as u16;
        self.config_read(bus, device, function, offset, size)
    }

    fn write_config_data(&mut self, port: u16, size: u8, value: u32) {
        if (self.config_address & 0x8000_0000) == 0 {
            return;
        }

        let bus = ((self.config_address >> 16) & 0xFF) as u8;
        let device = ((self.config_address >> 11) & 0x1F) as u8;
        let function = ((self.config_address >> 8) & 0x07) as u8;
        let register_base = (self.config_address & 0xFC) as u16;
        let port_offset = port - CONFIG_DATA_PORT;
        let offset = register_base + port_offset as u16;
        self.config_write(bus, device, function, offset, size, value);
    }
}
