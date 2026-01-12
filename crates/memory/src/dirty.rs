use crate::phys::GuestMemory;
use std::sync::{Arc, Mutex};

#[derive(Debug)]
struct DirtyBitmap {
    bits: Vec<u64>,
    pages: usize,
    page_size: u64,
}

impl DirtyBitmap {
    fn new(mem_len: u64, page_size: u32) -> Self {
        assert!(page_size != 0, "dirty tracking page_size must be non-zero");

        let page_size_u64 = u64::from(page_size);
        let pages_u64 = mem_len
            .checked_add(page_size_u64.saturating_sub(1))
            .unwrap_or(mem_len)
            / page_size_u64;
        let pages = usize::try_from(pages_u64).unwrap_or(0);
        let words = pages.div_ceil(64);
        Self {
            bits: vec![0u64; words],
            pages,
            page_size: page_size_u64,
        }
    }

    fn mark_range(&mut self, start: u64, len: usize) {
        if len == 0 || self.pages == 0 {
            return;
        }
        let end = start.saturating_add(len as u64).saturating_sub(1);
        let first_page = usize::try_from(start / self.page_size).unwrap_or(usize::MAX);
        let last_page = usize::try_from(end / self.page_size).unwrap_or(usize::MAX);
        if first_page >= self.pages {
            return;
        }
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

/// A reusable dirty-page tracker for guest RAM.
///
/// This is intentionally separate from any concrete RAM backend so it can be shared between
/// wrappers and snapshot adapters.
#[derive(Debug, Clone)]
pub struct DirtyTracker {
    inner: Arc<Mutex<DirtyBitmap>>,
    page_size: u32,
}

impl DirtyTracker {
    /// Create a new dirty tracker for `mem_len` bytes of RAM.
    pub fn new(mem_len: u64, page_size: u32) -> Self {
        assert!(page_size != 0, "dirty tracking page_size must be non-zero");
        Self {
            inner: Arc::new(Mutex::new(DirtyBitmap::new(mem_len, page_size))),
            page_size,
        }
    }

    /// Dirty page size in bytes.
    pub fn page_size(&self) -> u32 {
        self.page_size
    }

    /// Return and clear the set of dirty pages.
    ///
    /// Page indices are measured in units of the page size passed to [`DirtyTracker::new`].
    pub fn take_dirty_pages(&self) -> Vec<u64> {
        let Ok(mut bitmap) = self.inner.lock() else {
            return Vec::new();
        };
        bitmap.take()
    }

    /// Clear all dirty bits.
    pub fn clear_dirty(&self) {
        let Ok(mut bitmap) = self.inner.lock() else {
            return;
        };
        bitmap.clear();
    }

    /// Mark a guest-physical byte range as dirty.
    pub fn mark_range(&self, start: u64, len: usize) {
        let Ok(mut bitmap) = self.inner.lock() else {
            return;
        };
        bitmap.mark_range(start, len);
    }
}

/// Wrap a [`GuestMemory`] backend and mark dirty pages on all writes.
///
/// The returned [`DirtyTracker`] handle can be stored by higher-level code (e.g. snapshot
/// adapters) to drain/clear dirty state without needing to downcast the `GuestMemory` trait
/// object.
pub struct DirtyGuestMemory {
    inner: Box<dyn GuestMemory>,
    tracker: DirtyTracker,
}

impl DirtyGuestMemory {
    /// Wrap `inner` with dirty-page tracking and return both the wrapped memory and a tracker
    /// handle.
    pub fn new(inner: Box<dyn GuestMemory>, page_size: u32) -> (Self, DirtyTracker) {
        let tracker = DirtyTracker::new(inner.size(), page_size);
        (
            Self {
                inner,
                tracker: tracker.clone(),
            },
            tracker,
        )
    }

    /// Drain dirty pages.
    pub fn take_dirty_pages(&self) -> Vec<u64> {
        self.tracker.take_dirty_pages()
    }

    /// Clear dirty pages.
    pub fn clear_dirty(&self) {
        self.tracker.clear_dirty();
    }

    /// Borrow the shared tracker.
    pub fn tracker(&self) -> DirtyTracker {
        self.tracker.clone()
    }
}

impl GuestMemory for DirtyGuestMemory {
    fn size(&self) -> u64 {
        self.inner.size()
    }

    fn read_into(&self, paddr: u64, dst: &mut [u8]) -> crate::phys::GuestMemoryResult<()> {
        self.inner.read_into(paddr, dst)
    }

    fn write_from(&mut self, paddr: u64, src: &[u8]) -> crate::phys::GuestMemoryResult<()> {
        self.inner.write_from(paddr, src)?;
        self.tracker.mark_range(paddr, src.len());
        Ok(())
    }

    fn get_slice(&self, paddr: u64, len: usize) -> Option<&[u8]> {
        self.inner.get_slice(paddr, len)
    }

    fn get_slice_mut(&mut self, paddr: u64, len: usize) -> Option<&mut [u8]> {
        let slice = self.inner.get_slice_mut(paddr, len)?;
        // Conservatively mark the range dirty up-front since callers may mutate the returned slice
        // without going back through `write_from`.
        self.tracker.mark_range(paddr, len);
        Some(slice)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::phys::DenseMemory;

    const PAGE_SIZE: u32 = 4096;

    #[test]
    fn write_from_marks_pages_and_take_drains() {
        let inner = DenseMemory::new(3 * u64::from(PAGE_SIZE)).unwrap();
        let (mut mem, tracker) = DirtyGuestMemory::new(Box::new(inner), PAGE_SIZE);

        // Touch page 2 first to ensure the returned list is sorted.
        mem.write_from(0x2000, &[0xCC]).unwrap();

        // Cross a page boundary: last byte of page 0 + first byte of page 1.
        mem.write_from(0x0FFF, &[0xAA, 0xBB]).unwrap();

        assert_eq!(tracker.take_dirty_pages(), vec![0, 1, 2]);
        assert!(tracker.take_dirty_pages().is_empty());
    }

    #[test]
    fn get_slice_mut_marks_pages_dirty_conservatively() {
        let inner = DenseMemory::new(2 * u64::from(PAGE_SIZE)).unwrap();
        let (mut mem, tracker) = DirtyGuestMemory::new(Box::new(inner), PAGE_SIZE);

        assert!(tracker.take_dirty_pages().is_empty());

        // Request a mutable slice; even if the caller doesn't end up mutating it, the wrapper must
        // conservatively mark the page dirty.
        let slice = mem.get_slice_mut(0x1000, 4).unwrap();
        slice.copy_from_slice(&[1, 2, 3, 4]);

        assert_eq!(tracker.take_dirty_pages(), vec![1]);
    }

    #[test]
    fn reads_do_not_mark_pages_dirty() {
        let inner = DenseMemory::new(2 * u64::from(PAGE_SIZE)).unwrap();
        let (mut mem, tracker) = DirtyGuestMemory::new(Box::new(inner), PAGE_SIZE);

        mem.write_from(0x0, &[0xAA]).unwrap();
        assert_eq!(tracker.take_dirty_pages(), vec![0]);

        tracker.clear_dirty();
        let mut buf = [0u8; 1];
        mem.read_into(0x0, &mut buf).unwrap();
        assert!(tracker.take_dirty_pages().is_empty());
    }
}
