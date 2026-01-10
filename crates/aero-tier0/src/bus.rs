use std::any::Any;

use crate::interpreter::Exception;

pub const PAGE_SIZE: u64 = 4096;

/// Memory access interface used by the interpreter.
///
/// The interface includes a simple page versioning scheme. Any write increments
/// the version counter for the containing page, enabling code caches (JIT or
/// decoded-block caches) to cheaply validate cached translations.
pub trait CpuBus: Any {
    fn read_u8(&self, addr: u64) -> Result<u8, Exception>;
    fn write_u8(&mut self, addr: u64, val: u8) -> Result<(), Exception>;

    fn page_version(&self, page_base: u64) -> u64;

    fn as_any_mut(&mut self) -> &mut dyn Any;

    fn read_bytes(&self, addr: u64, dst: &mut [u8]) -> Result<(), Exception> {
        for (i, byte) in dst.iter_mut().enumerate() {
            *byte = self.read_u8(addr + i as u64)?;
        }
        Ok(())
    }

    fn write_bytes(&mut self, addr: u64, src: &[u8]) -> Result<(), Exception> {
        for (i, byte) in src.iter().enumerate() {
            self.write_u8(addr + i as u64, *byte)?;
        }
        Ok(())
    }
}

/// A simple in-memory bus implementation with page versioning.
#[derive(Clone)]
pub struct MemoryBus {
    mem: Vec<u8>,
    page_versions: Vec<u64>,
}

impl MemoryBus {
    pub fn new(size: usize) -> Self {
        let page_versions_len = (size as u64 + PAGE_SIZE - 1) / PAGE_SIZE;
        Self {
            mem: vec![0; size],
            page_versions: vec![0; page_versions_len as usize],
        }
    }

    pub fn load(&mut self, addr: u64, bytes: &[u8]) -> Result<(), Exception> {
        self.write_bytes(addr, bytes)
    }

    fn page_index(addr: u64) -> usize {
        (addr / PAGE_SIZE) as usize
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.mem
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.mem
    }

    pub fn bump_versions_for_write(&mut self, addr: u64, len: usize) {
        if len == 0 {
            return;
        }

        let start_page = addr & !(PAGE_SIZE - 1);
        let end_addr = addr.wrapping_add(len as u64 - 1);
        let end_page = end_addr & !(PAGE_SIZE - 1);

        let mut page = start_page;
        loop {
            let page_index = Self::page_index(page);
            if let Some(version) = self.page_versions.get_mut(page_index) {
                *version = version.wrapping_add(1);
            }
            if page == end_page {
                break;
            }
            page = page.wrapping_add(PAGE_SIZE);
        }
    }
}

impl CpuBus for MemoryBus {
    fn read_u8(&self, addr: u64) -> Result<u8, Exception> {
        self.mem
            .get(addr as usize)
            .copied()
            .ok_or(Exception::MemFault { addr })
    }

    fn write_u8(&mut self, addr: u64, val: u8) -> Result<(), Exception> {
        let slot = self
            .mem
            .get_mut(addr as usize)
            .ok_or(Exception::MemFault { addr })?;
        *slot = val;

        let page_index = Self::page_index(addr);
        if let Some(version) = self.page_versions.get_mut(page_index) {
            *version = version.wrapping_add(1);
        }
        Ok(())
    }

    fn page_version(&self, page_base: u64) -> u64 {
        let page_index = Self::page_index(page_base);
        self.page_versions.get(page_index).copied().unwrap_or(0)
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}
