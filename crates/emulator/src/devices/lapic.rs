use aero_devices::apic::LocalApic;
use std::sync::Arc;

use memory::MmioHandler;

/// MMIO adapter for [`LocalApic`] compatible with `memory::PhysicalMemoryBus`.
pub struct LapicMmio {
    lapic: Arc<LocalApic>,
}

impl LapicMmio {
    pub fn new(lapic: Arc<LocalApic>) -> Self {
        Self { lapic }
    }
}

impl MmioHandler for LapicMmio {
    fn read(&mut self, offset: u64, size: usize) -> u64 {
        if size == 0 {
            return 0;
        }
        let size = size.min(8);
        let mut buf = [0u8; 8];
        self.lapic.mmio_read(offset, &mut buf[..size]);
        u64::from_le_bytes(buf)
    }

    fn write(&mut self, offset: u64, size: usize, value: u64) {
        if size == 0 {
            return;
        }
        let size = size.min(8);
        let bytes = value.to_le_bytes();
        self.lapic.mmio_write(offset, &bytes[..size]);
    }
}
