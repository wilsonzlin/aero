//! Guest RAM backed directly by the wasm linear memory.
//!
//! The threaded wasm build caps the Rust allocator heap to the runtime-reserved region (see
//! `runtime_alloc.rs`) so allocating large guest RAM buffers on the heap is not viable.
//! Instead, the web runtime provisions a large shared `WebAssembly.Memory` and maps guest physical
//! address 0 at a fixed byte offset (`guest_base`) inside that linear memory.
//!
//! `WasmLinearGuestMemory` implements `memory::GuestMemory` by translating guest physical addresses
//! into linear-memory pointers and copying bytes with raw pointer operations.

#![cfg(target_arch = "wasm32")]

use memory::{GuestMemory, GuestMemoryError, GuestMemoryResult};

const WASM_PAGE_BYTES: u64 = 64 * 1024;

/// A [`GuestMemory`] implementation backed by the wasm linear memory.
#[derive(Debug, Clone)]
pub struct WasmLinearGuestMemory {
    /// Byte offset inside the wasm linear memory where guest physical address 0 begins.
    guest_base: u32,
    /// Guest RAM size in bytes.
    guest_size: u64,
}

impl WasmLinearGuestMemory {
    #[must_use]
    pub fn new(guest_base: u32, guest_size: u32) -> Self {
        Self {
            guest_base,
            guest_size: guest_size as u64,
        }
    }

    #[inline]
    fn wasm_memory_bytes() -> u64 {
        u64::from(core::arch::wasm32::memory_size(0) as u32) * WASM_PAGE_BYTES
    }

    #[inline]
    fn linear_addr(&self, paddr: u64, len: usize) -> GuestMemoryResult<u32> {
        let size = self.guest_size;
        let len_u64 = len as u64;

        let end_paddr = paddr
            .checked_add(len_u64)
            .ok_or(GuestMemoryError::OutOfRange { paddr, len, size })?;
        if end_paddr > size {
            return Err(GuestMemoryError::OutOfRange { paddr, len, size });
        }

        let base = u64::from(self.guest_base);
        let start = base
            .checked_add(paddr)
            .ok_or(GuestMemoryError::OutOfRange { paddr, len, size })?;
        let end = start
            .checked_add(len_u64)
            .ok_or(GuestMemoryError::OutOfRange { paddr, len, size })?;
        if end > Self::wasm_memory_bytes() {
            return Err(GuestMemoryError::OutOfRange { paddr, len, size });
        }

        u32::try_from(start).map_err(|_| GuestMemoryError::OutOfRange { paddr, len, size })
    }

    #[inline]
    fn check_empty_range(&self, paddr: u64) -> GuestMemoryResult<()> {
        let size = self.guest_size;
        if paddr > size {
            return Err(GuestMemoryError::OutOfRange {
                paddr,
                len: 0,
                size,
            });
        }

        let base = u64::from(self.guest_base);
        let start = base
            .checked_add(paddr)
            .ok_or(GuestMemoryError::OutOfRange {
                paddr,
                len: 0,
                size,
            })?;
        if start > Self::wasm_memory_bytes() {
            return Err(GuestMemoryError::OutOfRange {
                paddr,
                len: 0,
                size,
            });
        }
        Ok(())
    }
}

impl GuestMemory for WasmLinearGuestMemory {
    fn size(&self) -> u64 {
        self.guest_size
    }

    fn read_into(&self, paddr: u64, dst: &mut [u8]) -> GuestMemoryResult<()> {
        if dst.is_empty() {
            return self.check_empty_range(paddr);
        }

        let linear = self.linear_addr(paddr, dst.len())?;
        // Safety:
        // - `linear_addr` bounds-checks the range against the configured guest size and the current
        //   wasm memory size.
        // - `dst` is a valid mutable slice.
        unsafe {
            core::ptr::copy_nonoverlapping(linear as *const u8, dst.as_mut_ptr(), dst.len());
        }
        Ok(())
    }

    fn write_from(&mut self, paddr: u64, src: &[u8]) -> GuestMemoryResult<()> {
        if src.is_empty() {
            return self.check_empty_range(paddr);
        }

        let linear = self.linear_addr(paddr, src.len())?;
        // Safety:
        // - `linear_addr` bounds-checks the range against the configured guest size and the current
        //   wasm memory size.
        // - `src` is a valid slice.
        unsafe {
            core::ptr::copy_nonoverlapping(src.as_ptr(), linear as *mut u8, src.len());
        }
        Ok(())
    }
}

#[cfg(all(test, target_arch = "wasm32"))]
mod tests {
    use super::*;
    use crate::guest_layout;

    use wasm_bindgen_test::wasm_bindgen_test;

    fn alloc_guest_region_bytes(min_bytes: u32) -> (u32, u32) {
        let page_bytes = guest_layout::WASM_PAGE_BYTES as u32;
        let reserved_pages =
            (guest_layout::RUNTIME_RESERVED_BYTES as u32).div_ceil(page_bytes).max(1);
        let current_pages = core::arch::wasm32::memory_size(0) as u32;
        if current_pages < reserved_pages {
            let delta = reserved_pages - current_pages;
            let prev = core::arch::wasm32::memory_grow(0, delta as usize);
            assert_ne!(prev, usize::MAX, "memory.grow failed while reserving heap");
        }

        let pages = min_bytes.div_ceil(page_bytes).max(1);
        let before_pages = core::arch::wasm32::memory_size(0) as u32;
        let prev = core::arch::wasm32::memory_grow(0, pages as usize);
        assert_ne!(prev, usize::MAX, "memory.grow failed (requested {pages} pages)");

        let guest_base = before_pages * page_bytes;
        let guest_size = pages * page_bytes;
        (guest_base, guest_size)
    }

    #[wasm_bindgen_test]
    fn bounds_checks_and_basic_read_write() {
        let (guest_base, guest_size) = alloc_guest_region_bytes(0x1000);

        let mut mem = WasmLinearGuestMemory::new(guest_base, guest_size);

        mem.write_from(0, &[1, 2, 3, 4]).unwrap();
        let mut dst = [0u8; 4];
        mem.read_into(0, &mut dst).unwrap();
        assert_eq!(dst, [1, 2, 3, 4]);

        // Range crosses the configured guest_size.
        let mut oob = [0u8; 4];
        let err = mem
            .read_into(u64::from(guest_size - 2), &mut oob)
            .expect_err("expected OutOfRange");
        assert!(matches!(err, GuestMemoryError::OutOfRange { .. }));

        // Range fits guest_size but exceeds the actual wasm memory size.
        let (guest_base2, guest_size2) = alloc_guest_region_bytes(guest_size);
        let mut mem2 = WasmLinearGuestMemory::new(guest_base2, guest_size2.saturating_mul(2));
        let err = mem2
            .read_into(u64::from(guest_size2), &mut [0u8; 1])
            .expect_err("expected OutOfRange when wasm memory is too small");
        assert!(matches!(err, GuestMemoryError::OutOfRange { .. }));
    }
}

