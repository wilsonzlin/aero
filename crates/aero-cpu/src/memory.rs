use std::collections::HashSet;

use aero_core::memory::{Memory as CoreMemory, MemoryError};
use aero_jit_proto::block::CodeSource;

/// Flat guest memory used by the baseline CPU worker.
///
/// This is intentionally simplistic: no paging, no MMIO regions, no devices.
/// It's good enough to validate basic block discovery and Tier-1 compilation.
#[derive(Clone, Debug)]
pub struct Memory {
    bytes: Vec<u8>,
    code_pages: HashSet<u64>,
}

impl Memory {
    pub fn new(size: usize) -> Self {
        Self {
            bytes: vec![0; size],
            code_pages: HashSet::new(),
        }
    }

    pub fn load(&mut self, addr: u64, data: &[u8]) {
        let start = addr as usize;
        let end = start + data.len();
        self.bytes[start..end].copy_from_slice(data);
    }

    pub fn mark_code_pages(&mut self, pages: &[u64]) {
        for &p in pages {
            self.code_pages.insert(p);
        }
    }

    #[inline]
    fn in_bounds(&self, addr: u64, len: usize) -> bool {
        let start = addr as usize;
        start
            .checked_add(len)
            .map(|end| end <= self.bytes.len())
            .unwrap_or(false)
    }

    pub fn read_u8(&self, addr: u64) -> u8 {
        self.bytes[addr as usize]
    }

    pub fn read_u64(&self, addr: u64) -> u64 {
        let start = addr as usize;
        let mut buf = [0u8; 8];
        buf.copy_from_slice(&self.bytes[start..start + 8]);
        u64::from_le_bytes(buf)
    }

    /// Write a byte. Returns `true` if the write hit a page previously marked as code.
    pub fn write_u8(&mut self, addr: u64, val: u8) -> bool {
        self.bytes[addr as usize] = val;
        self.code_pages.contains(&(addr >> 12))
    }

    /// Write a u64. Returns `true` if the write hit a page previously marked as code.
    pub fn write_u64(&mut self, addr: u64, val: u64) -> bool {
        let start = addr as usize;
        self.bytes[start..start + 8].copy_from_slice(&val.to_le_bytes());
        let start_page = addr >> 12;
        let end_page = (addr + 7) >> 12;
        (start_page..=end_page).any(|p| self.code_pages.contains(&p))
    }
}

impl CodeSource for Memory {
    fn fetch_code(&self, addr: u64, len: usize) -> Option<&[u8]> {
        if !self.in_bounds(addr, len) {
            return None;
        }
        let start = addr as usize;
        Some(&self.bytes[start..start + len])
    }
}

impl CoreMemory for Memory {
    fn read(&self, addr: u64, buf: &mut [u8]) -> Result<(), MemoryError> {
        if !self.in_bounds(addr, buf.len()) {
            return Err(MemoryError::OutOfBounds {
                addr,
                len: buf.len(),
            });
        }
        let start = addr as usize;
        buf.copy_from_slice(&self.bytes[start..start + buf.len()]);
        Ok(())
    }

    fn write(&mut self, addr: u64, buf: &[u8]) -> Result<(), MemoryError> {
        if !self.in_bounds(addr, buf.len()) {
            return Err(MemoryError::OutOfBounds {
                addr,
                len: buf.len(),
            });
        }
        let start = addr as usize;
        self.bytes[start..start + buf.len()].copy_from_slice(buf);
        Ok(())
    }
}
