use aero_devices::apic::IoApic;
use std::sync::{Arc, Mutex};

use crate::memory_bus::MmioHandler;

/// Byte-addressable MMIO wrapper for [`IoApic`].
///
/// The IOAPIC programming model is based around 32-bit `IOREGSEL` and `IOWIN` registers.
/// This wrapper maps byte MMIO accesses onto 32-bit reads/writes by performing a
/// read-modify-write on the targeted 32-bit word.
///
/// This is sufficient for a minimal emulator memory bus that only exposes u8 MMIO handlers.
pub struct IoApicMmio {
    ioapic: Arc<Mutex<IoApic>>,
}

impl IoApicMmio {
    pub fn new(ioapic: Arc<Mutex<IoApic>>) -> Self {
        Self { ioapic }
    }
}

impl MmioHandler for IoApicMmio {
    fn read_u8(&mut self, offset: u64) -> u8 {
        let aligned = offset & !0x3;
        let shift = ((offset & 0x3) * 8) as u32;
        let mut ioapic = self.ioapic.lock().unwrap();
        let word = ioapic.mmio_read(aligned, 4) as u32;
        ((word >> shift) & 0xff) as u8
    }

    fn write_u8(&mut self, offset: u64, value: u8) {
        let aligned = offset & !0x3;
        let shift = ((offset & 0x3) * 8) as u32;
        let mask = 0xffu32 << shift;

        let mut ioapic = self.ioapic.lock().unwrap();
        let cur = ioapic.mmio_read(aligned, 4) as u32;
        let new = (cur & !mask) | (u32::from(value) << shift);
        ioapic.mmio_write(aligned, 4, u64::from(new));
    }
}

