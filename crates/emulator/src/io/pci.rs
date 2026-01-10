/// Minimal PCI interfaces required by the AHCI controller unit tests.
///
/// This is *not* a complete PCI subsystem. It exists to model config-space
/// behavior needed by operating systems (notably Windows) to bind the generic
/// AHCI driver based on the SATA/AHCI class code and BAR layout.
pub trait PciDevice {
    fn config_read(&self, offset: u16, size: usize) -> u32;
    fn config_write(&mut self, offset: u16, size: usize, value: u32);
}

use memory::MemoryBus;

pub trait MmioDevice {
    fn mmio_read(&mut self, mem: &mut dyn MemoryBus, offset: u64, size: usize) -> u32;
    fn mmio_write(&mut self, mem: &mut dyn MemoryBus, offset: u64, size: usize, value: u32);
}

#[derive(Clone, Debug)]
pub struct PciConfigSpace {
    data: [u8; 256],
}

impl PciConfigSpace {
    pub fn new() -> Self {
        Self { data: [0; 256] }
    }

    pub fn set_u16(&mut self, offset: usize, value: u16) {
        self.data[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    pub fn set_u32(&mut self, offset: usize, value: u32) {
        self.data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    pub fn read(&self, offset: u16, size: usize) -> u32 {
        let offset = offset as usize;
        match size {
            1 => self.data[offset] as u32,
            2 => u16::from_le_bytes(self.data[offset..offset + 2].try_into().unwrap()) as u32,
            4 => u32::from_le_bytes(self.data[offset..offset + 4].try_into().unwrap()),
            _ => 0,
        }
    }

    pub fn write(&mut self, offset: u16, size: usize, value: u32) {
        let offset = offset as usize;
        match size {
            1 => self.data[offset] = value as u8,
            2 => self.data[offset..offset + 2].copy_from_slice(&(value as u16).to_le_bytes()),
            4 => self.data[offset..offset + 4].copy_from_slice(&value.to_le_bytes()),
            _ => {}
        }
    }
}
