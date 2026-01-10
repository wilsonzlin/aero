/// Abstraction for guest physical memory access.
///
/// The MMU performs page table walks by reading and writing guest physical
/// memory. Real systems may map page tables to RAM or MMIO; therefore reads are
/// defined as `&mut self` to allow implementations with side effects.
pub trait MemoryBus {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]);
    fn write_physical(&mut self, paddr: u64, buf: &[u8]);

    fn read_u8(&mut self, paddr: u64) -> u8 {
        let mut buf = [0u8; 1];
        self.read_physical(paddr, &mut buf);
        buf[0]
    }

    fn read_u16(&mut self, paddr: u64) -> u16 {
        let mut buf = [0u8; 2];
        self.read_physical(paddr, &mut buf);
        u16::from_le_bytes(buf)
    }

    fn read_u32(&mut self, paddr: u64) -> u32 {
        let mut buf = [0u8; 4];
        self.read_physical(paddr, &mut buf);
        u32::from_le_bytes(buf)
    }

    fn write_u8(&mut self, paddr: u64, val: u8) {
        self.write_physical(paddr, &[val]);
    }

    fn write_u16(&mut self, paddr: u64, val: u16) {
        self.write_physical(paddr, &val.to_le_bytes());
    }

    fn write_u32(&mut self, paddr: u64, val: u32) {
        self.write_physical(paddr, &val.to_le_bytes());
    }
}

