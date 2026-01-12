#![forbid(unsafe_code)]

use memory::{GuestMemory, GuestMemoryResult};
use std::sync::{Arc, Mutex};

/// Default guest page size used by Aero snapshots when dirty-page tracking is enabled.
pub const DEFAULT_DIRTY_PAGE_SIZE: u32 = 4096;

#[derive(Debug)]
struct DirtyBitmap {
    bits: Vec<u64>,
    pages: usize,
    page_size: usize,
}

impl DirtyBitmap {
    fn new(mem_len: u64, page_size: u32) -> Self {
        assert!(page_size != 0, "dirty tracking page_size must be non-zero");

        let page_size_usize = page_size as usize;

        let pages = usize::try_from(
            mem_len
                .checked_add((page_size as u64).saturating_sub(1))
                .unwrap_or(mem_len)
                / (page_size as u64),
        )
        .unwrap_or(0);

        let words = pages.div_ceil(64);
        Self {
            bits: vec![0u64; words],
            pages,
            page_size: page_size_usize,
        }
    }

    fn mark_range(&mut self, start: u64, len: usize) {
        if len == 0 || self.pages == 0 {
            return;
        }

        let len_u64 = len as u64;
        let end = start.saturating_add(len_u64).saturating_sub(1);

        let first_page = usize::try_from(start / self.page_size as u64).unwrap_or(usize::MAX);
        if first_page >= self.pages {
            return;
        }
        let last_page = usize::try_from(end / self.page_size as u64).unwrap_or(usize::MAX);
        let last_page = last_page.min(self.pages.saturating_sub(1));

        for page in first_page..=last_page {
            let word = page / 64;
            let bit = page % 64;
            if let Some(slot) = self.bits.get_mut(word) {
                *slot |= 1u64 << bit;
            }
        }
    }

    fn take(&mut self) -> Vec<u64> {
        let mut pages = Vec::new();

        for (word_idx, word) in self.bits.iter_mut().enumerate() {
            let mut w = *word;
            if w == 0 {
                continue;
            }

            // Clear the word up-front so we're resilient to panics while scanning.
            *word = 0;

            while w != 0 {
                let bit = w.trailing_zeros() as usize;
                let page = word_idx * 64 + bit;
                if page < self.pages {
                    pages.push(page as u64);
                }
                w &= !(1u64 << bit);
            }
        }

        pages
    }

    fn clear(&mut self) {
        self.bits.fill(0);
    }
}

/// Cloneable handle for reading and clearing dirty pages from a [`DirtyTrackingMemory`].
#[derive(Clone)]
pub struct DirtyTrackingHandle {
    page_size: u32,
    bitmap: Arc<Mutex<DirtyBitmap>>,
}

impl DirtyTrackingHandle {
    pub fn page_size(&self) -> u32 {
        self.page_size
    }

    fn mark_range(&self, start: u64, len: usize) {
        let Ok(mut bitmap) = self.bitmap.lock() else {
            return;
        };
        bitmap.mark_range(start, len);
    }

    pub fn take_dirty_pages(&self) -> Vec<u64> {
        let Ok(mut bitmap) = self.bitmap.lock() else {
            return Vec::new();
        };
        bitmap.take()
    }

    pub fn clear_dirty(&self) {
        let Ok(mut bitmap) = self.bitmap.lock() else {
            return;
        };
        bitmap.clear();
    }
}

/// Wrapper around a [`memory::GuestMemory`] backend that tracks guest pages written since the last
/// `take_dirty_pages()` call.
///
/// This is intended for dirty-page snapshots (`aero_snapshot::RamMode::Dirty`), including writes
/// originating from device/DMA accesses through the platform memory bus.
pub struct DirtyTrackingMemory {
    inner: Box<dyn GuestMemory>,
    page_size: u32,
    bitmap: Arc<Mutex<DirtyBitmap>>,
}

impl DirtyTrackingMemory {
    pub fn new(inner: Box<dyn GuestMemory>, page_size: u32) -> Self {
        let bitmap = Arc::new(Mutex::new(DirtyBitmap::new(inner.size(), page_size)));
        Self {
            inner,
            page_size,
            bitmap,
        }
    }

    pub fn tracking_handle(&self) -> DirtyTrackingHandle {
        DirtyTrackingHandle {
            page_size: self.page_size,
            bitmap: Arc::clone(&self.bitmap),
        }
    }

    pub fn take_dirty_pages(&mut self) -> Vec<u64> {
        self.tracking_handle().take_dirty_pages()
    }

    pub fn clear_dirty(&mut self) {
        self.tracking_handle().clear_dirty()
    }
}

impl GuestMemory for DirtyTrackingMemory {
    fn size(&self) -> u64 {
        self.inner.size()
    }

    fn read_into(&self, paddr: u64, dst: &mut [u8]) -> GuestMemoryResult<()> {
        self.inner.read_into(paddr, dst)
    }

    fn write_from(&mut self, paddr: u64, src: &[u8]) -> GuestMemoryResult<()> {
        self.inner.write_from(paddr, src)?;

        // Writes performed through the guest memory abstraction represent guest RAM modifications,
        // so mark the corresponding guest pages as dirty.
        let handle = self.tracking_handle();
        handle.mark_range(paddr, src.len());

        Ok(())
    }

    fn get_slice(&self, paddr: u64, len: usize) -> Option<&[u8]> {
        self.inner.get_slice(paddr, len)
    }

    fn get_slice_mut(&mut self, paddr: u64, len: usize) -> Option<&mut [u8]> {
        // Mark pages as dirty *before* returning the mutable slice (conservative but safe), since
        // mutations may occur via the returned slice without going through `write_from`.
        let handle = self.tracking_handle();

        let slice = self.inner.get_slice_mut(paddr, len)?;
        handle.mark_range(paddr, len);
        Some(slice)
    }
}
