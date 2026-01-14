use aero_machine::{Machine, MachineConfig};
use memory::{DenseMemory, GuestMemory, GuestMemoryResult};

#[derive(Debug)]
struct MarkedGuestMemory {
    #[allow(dead_code)]
    marker: u32,
    inner: DenseMemory,
}

impl MarkedGuestMemory {
    fn new(size: u64, marker: u32) -> Self {
        Self {
            marker,
            inner: DenseMemory::new(size).expect("DenseMemory::new should succeed for test size"),
        }
    }
}

impl GuestMemory for MarkedGuestMemory {
    fn size(&self) -> u64 {
        self.inner.size()
    }

    fn read_into(&self, paddr: u64, dst: &mut [u8]) -> GuestMemoryResult<()> {
        self.inner.read_into(paddr, dst)
    }

    fn write_from(&mut self, paddr: u64, src: &[u8]) -> GuestMemoryResult<()> {
        self.inner.write_from(paddr, src)
    }

    fn get_slice(&self, paddr: u64, len: usize) -> Option<&[u8]> {
        self.inner.get_slice(paddr, len)
    }

    fn get_slice_mut(&mut self, paddr: u64, len: usize) -> Option<&mut [u8]> {
        self.inner.get_slice_mut(paddr, len)
    }
}

#[test]
fn machine_constructs_with_custom_guest_memory() {
    let cfg = MachineConfig {
        ram_size_bytes: 8 * 1024 * 1024,
        ..Default::default()
    };
    let backing = MarkedGuestMemory::new(cfg.ram_size_bytes, 0x126);

    let mut machine = Machine::new_with_guest_memory(cfg, Box::new(backing)).unwrap();

    // Ensure the machine can be reset multiple times without panicking.
    machine.reset();
}
