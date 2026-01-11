use core::ops::Range;

/// Minimal guest-physical memory interface used by the HDA controller for DMA.
///
/// The real emulator will likely provide a richer bus abstraction. Keeping this
/// crate on a tiny trait makes it easy to unit test the CORB/RIRB and stream DMA
/// logic without pulling in unrelated subsystems.
pub trait MemoryAccess {
    fn read_physical(&self, addr: u64, buf: &mut [u8]);
    fn write_physical(&mut self, addr: u64, buf: &[u8]);

    fn read_u8(&self, addr: u64) -> u8 {
        let mut b = [0u8; 1];
        self.read_physical(addr, &mut b);
        b[0]
    }

    fn read_u16(&self, addr: u64) -> u16 {
        let mut b = [0u8; 2];
        self.read_physical(addr, &mut b);
        u16::from_le_bytes(b)
    }

    fn read_u32(&self, addr: u64) -> u32 {
        let mut b = [0u8; 4];
        self.read_physical(addr, &mut b);
        u32::from_le_bytes(b)
    }

    fn read_u64(&self, addr: u64) -> u64 {
        let mut b = [0u8; 8];
        self.read_physical(addr, &mut b);
        u64::from_le_bytes(b)
    }

    fn write_u8(&mut self, addr: u64, val: u8) {
        self.write_physical(addr, &[val]);
    }

    fn write_u16(&mut self, addr: u64, val: u16) {
        self.write_physical(addr, &val.to_le_bytes());
    }

    fn write_u32(&mut self, addr: u64, val: u32) {
        self.write_physical(addr, &val.to_le_bytes());
    }

    fn write_u64(&mut self, addr: u64, val: u64) {
        self.write_physical(addr, &val.to_le_bytes());
    }
}

/// Simple contiguous guest memory implementation for tests.
#[derive(Clone)]
pub struct GuestMemory {
    data: Vec<u8>,
}

impl GuestMemory {
    pub fn new(size: usize) -> Self {
        Self {
            data: vec![0; size],
        }
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.data
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.data
    }

    fn range(&self, addr: u64, len: usize) -> Range<usize> {
        let start = addr as usize;
        let end = start.checked_add(len).expect("address overflow");
        assert!(end <= self.data.len(), "guest memory out of bounds");
        start..end
    }
}

impl MemoryAccess for GuestMemory {
    fn read_physical(&self, addr: u64, buf: &mut [u8]) {
        let r = self.range(addr, buf.len());
        buf.copy_from_slice(&self.data[r]);
    }

    fn write_physical(&mut self, addr: u64, buf: &[u8]) {
        let r = self.range(addr, buf.len());
        self.data[r].copy_from_slice(buf);
    }
}
