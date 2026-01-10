use std::fmt;

/// Round `value` up to the nearest multiple of `alignment`.
///
/// `alignment` must be > 0.
pub(crate) fn align_up(value: u64, alignment: u64) -> u64 {
    debug_assert!(alignment > 0);

    // `value + alignment - 1` can overflow if the user passes pathological
    // inputs, so use a checked path and fall back to saturating behaviour.
    let add = alignment - 1;
    match value.checked_add(add) {
        Some(v) => v / alignment * alignment,
        None => u64::MAX / alignment * alignment,
    }
}

/// A simple linear allocator for sub-allocating a fixed byte range.
///
/// This is intentionally CPU-only: it tracks offsets, not actual GPU memory.
#[derive(Clone)]
pub struct BufferArena {
    base: u64,
    capacity: u64,
    cursor: u64,
}

impl BufferArena {
    /// Create an arena that allocates offsets in `[base, base + capacity)`.
    pub fn new(base: u64, capacity: u64) -> Self {
        Self {
            base,
            capacity,
            cursor: base,
        }
    }

    /// Reset the arena cursor back to the base.
    pub fn reset(&mut self) {
        self.cursor = self.base;
    }

    /// Total capacity in bytes.
    pub fn capacity(&self) -> u64 {
        self.capacity
    }

    /// The base offset for allocations from this arena.
    pub fn base(&self) -> u64 {
        self.base
    }

    /// Bytes remaining until the arena is full.
    pub fn remaining(&self) -> u64 {
        self.end().saturating_sub(self.cursor)
    }

    /// Current cursor (next allocation will be at or after this offset).
    pub fn cursor(&self) -> u64 {
        self.cursor
    }

    fn end(&self) -> u64 {
        self.base + self.capacity
    }

    /// Allocate `size` bytes with `alignment`.
    ///
    /// Returns the absolute byte offset (from the start of the underlying
    /// buffer) on success.
    pub fn alloc(&mut self, size: u64, alignment: u64) -> Option<u64> {
        let alignment = alignment.max(1);
        let size = size;

        let aligned = align_up(self.cursor, alignment);
        debug_assert_eq!(aligned % alignment, 0);

        let end = aligned.checked_add(size)?;
        if end > self.end() {
            return None;
        }

        self.cursor = end;
        Some(aligned)
    }
}

impl fmt::Debug for BufferArena {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BufferArena")
            .field("base", &self.base)
            .field("capacity", &self.capacity)
            .field("cursor", &self.cursor)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn align_up_rounds_to_multiple() {
        assert_eq!(align_up(0, 4), 0);
        assert_eq!(align_up(1, 4), 4);
        assert_eq!(align_up(4, 4), 4);
        assert_eq!(align_up(5, 4), 8);
        assert_eq!(align_up(255, 256), 256);
        assert_eq!(align_up(256, 256), 256);
    }

    #[test]
    fn arena_alloc_respects_alignment_and_capacity() {
        let mut arena = BufferArena::new(0, 64);

        let a = arena.alloc(1, 1).unwrap();
        assert_eq!(a, 0);

        let b = arena.alloc(1, 16).unwrap();
        assert_eq!(b, 16);

        // 48 bytes remaining (17..64), next 32-byte aligned allocation is 32.
        let c = arena.alloc(16, 32).unwrap();
        assert_eq!(c, 32);

        // Not enough space for another 33 bytes.
        assert!(arena.alloc(33, 1).is_none());
    }

    #[test]
    fn arena_reset_reuses_space() {
        let mut arena = BufferArena::new(128, 64);
        assert_eq!(arena.alloc(8, 4).unwrap(), 128);
        assert_eq!(arena.alloc(8, 4).unwrap(), 136);

        arena.reset();
        assert_eq!(arena.alloc(8, 4).unwrap(), 128);
    }
}
