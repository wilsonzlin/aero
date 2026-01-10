use std::fmt;

/// Minimal guest physical memory interface used for DMA.
///
/// A full system emulator will likely expose richer APIs (paging-aware accesses, MMIO, etc).
/// Storage devices only need a raw physical read/write view.
pub trait GuestMemory {
    fn read(&self, paddr: u64, buf: &mut [u8]);
    fn write(&mut self, paddr: u64, buf: &[u8]);
}

pub trait GuestMemoryExt: GuestMemory {
    fn read_u8(&self, paddr: u64) -> u8 {
        let mut b = [0u8; 1];
        self.read(paddr, &mut b);
        b[0]
    }

    fn read_u16(&self, paddr: u64) -> u16 {
        let mut b = [0u8; 2];
        self.read(paddr, &mut b);
        u16::from_le_bytes(b)
    }

    fn read_u32(&self, paddr: u64) -> u32 {
        let mut b = [0u8; 4];
        self.read(paddr, &mut b);
        u32::from_le_bytes(b)
    }

    fn read_u64(&self, paddr: u64) -> u64 {
        let mut b = [0u8; 8];
        self.read(paddr, &mut b);
        u64::from_le_bytes(b)
    }

    fn write_u8(&mut self, paddr: u64, val: u8) {
        self.write(paddr, &[val]);
    }

    fn write_u16(&mut self, paddr: u64, val: u16) {
        self.write(paddr, &val.to_le_bytes());
    }

    fn write_u32(&mut self, paddr: u64, val: u32) {
        self.write(paddr, &val.to_le_bytes());
    }

    fn write_u64(&mut self, paddr: u64, val: u64) {
        self.write(paddr, &val.to_le_bytes());
    }
}

impl<T: GuestMemory + ?Sized> GuestMemoryExt for T {}

/// Interrupt line used by devices that signal legacy INTx-style interrupts.
pub trait IrqLine {
    fn set_level(&self, high: bool);
}

#[derive(Default)]
struct TestIrqLineState {
    level: bool,
    transitions: Vec<bool>,
}

/// A simple, shareable IRQ line for unit tests.
#[derive(Clone, Default)]
pub struct TestIrqLine(std::sync::Arc<std::sync::Mutex<TestIrqLineState>>);

impl TestIrqLine {
    pub fn level(&self) -> bool {
        self.0.lock().unwrap().level
    }

    pub fn transitions(&self) -> Vec<bool> {
        self.0.lock().unwrap().transitions.clone()
    }
}

impl fmt::Debug for TestIrqLine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.0.lock().unwrap();
        f.debug_struct("TestIrqLine")
            .field("level", &state.level)
            .field("transitions", &state.transitions)
            .finish()
    }
}

impl IrqLine for TestIrqLine {
    fn set_level(&self, high: bool) {
        let mut state = self.0.lock().unwrap();
        if state.level != high {
            state.level = high;
            state.transitions.push(high);
        }
    }
}

#[derive(Clone)]
pub struct TestMemory {
    data: Vec<u8>,
}

impl TestMemory {
    pub fn new(size: usize) -> Self {
        Self { data: vec![0; size] }
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.data
    }

    fn check_range(&self, paddr: u64, len: usize) {
        let start = paddr as usize;
        let end = start.checked_add(len).expect("guest memory address overflow");
        assert!(end <= self.data.len(), "guest memory OOB access");
    }
}

impl GuestMemory for TestMemory {
    fn read(&self, paddr: u64, buf: &mut [u8]) {
        self.check_range(paddr, buf.len());
        let start = paddr as usize;
        buf.copy_from_slice(&self.data[start..start + buf.len()]);
    }

    fn write(&mut self, paddr: u64, buf: &[u8]) {
        self.check_range(paddr, buf.len());
        let start = paddr as usize;
        self.data[start..start + buf.len()].copy_from_slice(buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_memory_read_write_primitives() {
        let mut mem = TestMemory::new(64);
        mem.write_u32(4, 0x1122_3344);
        mem.write_u64(8, 0xAABB_CCDD_EEFF_0011);
        assert_eq!(mem.read_u32(4), 0x1122_3344);
        assert_eq!(mem.read_u64(8), 0xAABB_CCDD_EEFF_0011);
    }

    #[test]
    fn irq_transitions_recorded() {
        let irq = TestIrqLine::default();
        irq.set_level(true);
        irq.set_level(true);
        irq.set_level(false);
        assert_eq!(irq.level(), false);
        assert_eq!(irq.transitions(), vec![true, false]);
    }
}
