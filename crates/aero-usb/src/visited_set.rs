use alloc::vec;
use alloc::vec::Vec;

/// A small open-addressing set for `u32` keys backed by a single contiguous allocation.
///
/// This is used by UHCI/EHCI schedule walkers for cycle detection. Guest-controlled schedules can
/// be large; using `Vec::contains()` would make the cycle check O(n²) and let a malicious guest burn
/// disproportionate host CPU while still staying within traversal budgets.
///
/// Invariants:
/// - `0` is reserved as an empty-slot sentinel. Callers must not insert `0`.
#[derive(Clone, Debug)]
pub(crate) struct VisitedSet {
    table: Vec<u32>,
    mask: usize,
}

impl VisitedSet {
    /// Create a set sized to hold up to `max_elems` inserts with a low load factor.
    pub(crate) fn new(max_elems: usize) -> Self {
        // Keep the table at <= 50% load so linear probing stays fast and we always have an empty
        // slot (prevents infinite loops).
        let want = max_elems.saturating_mul(2).max(16);
        let size = want
            .checked_next_power_of_two()
            .unwrap_or(16)
            .max(16);
        Self {
            table: vec![0; size],
            mask: size - 1,
        }
    }

    #[inline]
    fn hash(key: u32) -> usize {
        // Multiplicative hash (Knuth). For the power-of-two table sizes we use, this distributes
        // aligned physical addresses reasonably well.
        key.wrapping_mul(0x9E37_79B1) as usize
    }

    /// Returns `true` if `key` is present.
    pub(crate) fn contains(&self, key: u32) -> bool {
        debug_assert!(key != 0, "VisitedSet key must be non-zero");
        let mut idx = Self::hash(key) & self.mask;
        loop {
            let slot = self.table[idx];
            if slot == 0 {
                return false;
            }
            if slot == key {
                return true;
            }
            idx = (idx + 1) & self.mask;
        }
    }

    /// Insert `key`. Returns `true` if it was already present.
    pub(crate) fn insert(&mut self, key: u32) -> bool {
        debug_assert!(key != 0, "VisitedSet key must be non-zero");
        let mut idx = Self::hash(key) & self.mask;
        loop {
            let slot = self.table[idx];
            if slot == 0 {
                self.table[idx] = key;
                return false;
            }
            if slot == key {
                return true;
            }
            idx = (idx + 1) & self.mask;
        }
    }
}
