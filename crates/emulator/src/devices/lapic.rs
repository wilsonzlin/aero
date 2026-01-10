use aero_devices::apic::LocalApic;
use std::sync::Arc;

use crate::memory_bus::MmioHandler;

/// Byte-addressable MMIO wrapper for [`LocalApic`].
///
/// The emulator's minimal memory bus routes MMIO via per-byte callbacks, but the LAPIC
/// specification is register-oriented with 32-bit accesses being the common case.
///
/// [`LocalApic`] itself already implements a byte-granular MMIO slice interface, so this
/// wrapper simply adapts it to the `read_u8` / `write_u8` bus trait.
pub struct LapicMmio {
    lapic: Arc<LocalApic>,
}

impl LapicMmio {
    pub fn new(lapic: Arc<LocalApic>) -> Self {
        Self { lapic }
    }
}

impl MmioHandler for LapicMmio {
    fn read_u8(&mut self, offset: u64) -> u8 {
        let mut buf = [0u8; 1];
        self.lapic.mmio_read(offset, &mut buf);
        buf[0]
    }

    fn write_u8(&mut self, offset: u64, value: u8) {
        self.lapic.mmio_write(offset, &[value]);
    }
}

