use aero_devices::apic::IoApic;
use std::sync::{Arc, Mutex};

use memory::MmioHandler;

/// MMIO adapter for [`IoApic`] compatible with `memory::PhysicalMemoryBus`.
///
/// The IOAPIC programming model is based around 32-bit `IOREGSEL` and `IOWIN` registers.
/// This adapter therefore implements sub-32-bit accesses by performing a read-modify-write on
/// the containing 32-bit word.
pub struct IoApicMmio {
    ioapic: Arc<Mutex<IoApic>>,
}

impl IoApicMmio {
    pub fn new(ioapic: Arc<Mutex<IoApic>>) -> Self {
        Self { ioapic }
    }

    fn read_byte(&mut self, offset: u64) -> u8 {
        let aligned = offset & !0x3;
        let shift = ((offset & 0x3) * 8) as u32;
        let mut ioapic = self.ioapic.lock().unwrap();
        let word = ioapic.mmio_read(aligned, 4) as u32;
        ((word >> shift) & 0xff) as u8
    }

    fn write_byte(&mut self, offset: u64, value: u8) {
        let aligned = offset & !0x3;
        let shift = ((offset & 0x3) * 8) as u32;
        let mask = 0xffu32 << shift;

        let mut ioapic = self.ioapic.lock().unwrap();
        let cur = ioapic.mmio_read(aligned, 4) as u32;
        let new = (cur & !mask) | (u32::from(value) << shift);
        ioapic.mmio_write(aligned, 4, u64::from(new));
    }
}

impl MmioHandler for IoApicMmio {
    fn read(&mut self, offset: u64, size: usize) -> u64 {
        if size == 0 {
            return 0;
        }
        let size = size.min(8);
        let mut out = 0u64;
        for i in 0..size {
            let byte = self.read_byte(offset.wrapping_add(i as u64));
            out |= (byte as u64) << (i * 8);
        }
        out
    }

    fn write(&mut self, offset: u64, size: usize, value: u64) {
        if size == 0 {
            return;
        }
        let size = size.min(8);
        for i in 0..size {
            let byte = ((value >> (i * 8)) & 0xff) as u8;
            self.write_byte(offset.wrapping_add(i as u64), byte);
        }
    }
}
