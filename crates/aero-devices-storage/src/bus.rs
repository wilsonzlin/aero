pub use aero_devices::irq::IrqLine;
use std::fmt;

use memory::MemoryBus;
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
        Self {
            data: vec![0; size],
        }
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.data
    }

    fn check_range(&self, paddr: u64, len: usize) {
        let start = paddr as usize;
        let end = start
            .checked_add(len)
            .expect("guest memory address overflow");
        assert!(end <= self.data.len(), "guest memory OOB access");
    }
}

impl MemoryBus for TestMemory {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        self.check_range(paddr, buf.len());
        let start = paddr as usize;
        buf.copy_from_slice(&self.data[start..start + buf.len()]);
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        self.check_range(paddr, buf.len());
        let start = paddr as usize;
        self.data[start..start + buf.len()].copy_from_slice(buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use memory::MemoryBus;

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
        assert!(!irq.level());
        assert_eq!(irq.transitions(), vec![true, false]);
    }
}
