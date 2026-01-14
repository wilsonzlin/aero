/// Minimal PCI interfaces required by legacy emulator compatibility shims.
///
/// This is *not* a complete PCI subsystem, and it is **not** part of the canonical
/// VM wiring stack.
///
/// Canonical PCI lives in `crates/devices` (`aero_devices::pci::*`) and is used by
/// `crates/aero-machine` / `crates/aero-pc-platform`.
///
/// This module exists primarily to model config-space behavior needed by older
/// controller wrappers and unit tests (notably Windows binding the generic AHCI
/// driver based on the SATA/AHCI class code and BAR layout).
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

    pub fn set_u8(&mut self, offset: usize, value: u8) {
        self.data[offset] = value;
    }

    pub fn set_u16(&mut self, offset: usize, value: u16) {
        self.data[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    pub fn set_u32(&mut self, offset: usize, value: u32) {
        self.data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    pub fn read(&self, offset: u16, size: usize) -> u32 {
        let offset = offset as usize;
        if !matches!(size, 1 | 2 | 4) {
            return 0;
        }
        if offset
            .checked_add(size)
            .is_none_or(|end| end > self.data.len())
        {
            return 0;
        }
        match size {
            1 => self.data[offset] as u32,
            2 => u16::from_le_bytes(self.data[offset..offset + 2].try_into().unwrap()) as u32,
            4 => u32::from_le_bytes(self.data[offset..offset + 4].try_into().unwrap()),
            _ => 0,
        }
    }

    pub fn write(&mut self, offset: u16, size: usize, value: u32) {
        let offset = offset as usize;
        if !matches!(size, 1 | 2 | 4) {
            return;
        }
        if offset
            .checked_add(size)
            .is_none_or(|end| end > self.data.len())
        {
            return;
        }

        // PCI Status register bytes (0x06..=0x07) are read-only / RW1C on real hardware. Guests
        // commonly write the Command register using a 32-bit store at 0x04 with zeros in the upper
        // 16 bits; such writes must not clobber device-managed status bits.
        //
        // Treat the Status bytes as write-ignored here to keep legacy emulator wrappers closer to
        // real PCI behavior.
        let status_range = 0x06..0x08;
        // Header Type (0x0E) and Interrupt Pin (0x3D) are read-only. Guests may issue wider writes
        // that overlap these bytes (e.g. dword stores at 0x0C or 0x3C); those writes must not
        // clobber device-reported values.
        let header_type = 0x0e;
        let interrupt_pin = 0x3d;

        for i in 0..size {
            let addr = offset + i;
            if status_range.contains(&addr) || addr == header_type || addr == interrupt_pin {
                continue;
            }
            self.data[addr] = ((value >> (8 * i)) & 0xFF) as u8;
        }
    }
}

impl Default for PciConfigSpace {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::PciConfigSpace;

    #[test]
    fn config_space_oob_accesses_do_not_panic() {
        let mut cfg = PciConfigSpace::new();

        assert_eq!(cfg.read(0x100, 1), 0);
        assert_eq!(cfg.read(0xff, 2), 0);
        assert_eq!(cfg.read(0xfe, 4), 0);
        assert_eq!(cfg.read(0, 3), 0);

        cfg.write(0x100, 1, 0x12);
        cfg.write(0xff, 2, 0x1234);
        cfg.write(0xfe, 4, 0x1234_5678);
        cfg.write(0, 3, 0xDEAD_BEEF);
    }

    #[test]
    fn dword_command_write_does_not_clobber_status_register() {
        let mut cfg = PciConfigSpace::new();
        cfg.set_u16(0x06, 0x1234);

        // Common pattern: 32-bit write at 0x04 with upper half (Status) = 0.
        cfg.write(0x04, 4, 0x0000_0006);

        assert_eq!(cfg.read(0x06, 2), 0x1234);
        assert_eq!(cfg.read(0x04, 2), 0x0006);
    }

    #[test]
    fn dword_interrupt_line_write_does_not_clobber_interrupt_pin() {
        let mut cfg = PciConfigSpace::new();
        cfg.set_u8(0x3d, 1);

        // Common pattern: 32-bit write at 0x3C with upper bytes (Interrupt Pin and reserved) = 0.
        cfg.write(0x3c, 4, 0x0000_000a);

        assert_eq!(cfg.read(0x3d, 1), 1);
        assert_eq!(cfg.read(0x3c, 1), 0x0a);
    }

    #[test]
    fn dword_cache_line_write_does_not_clobber_header_type() {
        let mut cfg = PciConfigSpace::new();
        cfg.set_u8(0x0e, 0x80);

        // Dword store at 0x0C spans cache-line/latency/header-type/bist. Header Type is read-only.
        cfg.write(0x0c, 4, 0x12_00_11_22);

        assert_eq!(cfg.read(0x0e, 1), 0x80);
        assert_eq!(cfg.read(0x0c, 1), 0x22);
        assert_eq!(cfg.read(0x0d, 1), 0x11);
        assert_eq!(cfg.read(0x0f, 1), 0x12);
    }
}
