use serde::{Deserialize, Serialize};

/// A half-open byte range `[start, end)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ByteRange {
    pub start: u64,
    pub end: u64,
}

impl ByteRange {
    pub fn new(start: u64, end: u64) -> Self {
        // Caller is allowed to pass an invalid (reversed) range; it will be treated as empty via
        // `ByteRange::is_empty` / `ByteRange::len`. Avoid panicking in debug/fuzz builds.
        Self { start, end }
    }

    pub fn len(&self) -> u64 {
        self.end.saturating_sub(self.start)
    }

    pub fn is_empty(&self) -> bool {
        self.start >= self.end
    }

    fn overlaps_or_adjacent(&self, other: &ByteRange) -> bool {
        self.start <= other.end && other.start <= self.end
    }

    fn merge(&self, other: &ByteRange) -> ByteRange {
        ByteRange {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }
}

/// A set of disjoint, sorted byte ranges.
///
/// Invariants:
/// - Ranges are stored in ascending order.
/// - No ranges overlap or touch (adjacent ranges are merged).
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RangeSet {
    ranges: Vec<ByteRange>,
}

impl RangeSet {
    pub fn new() -> Self {
        Self { ranges: Vec::new() }
    }

    pub fn ranges(&self) -> &[ByteRange] {
        &self.ranges
    }

    pub fn total_len(&self) -> u64 {
        self.ranges.iter().map(ByteRange::len).sum()
    }

    pub fn contains_range(&self, start: u64, end: u64) -> bool {
        if start >= end {
            return true;
        }
        match self.ranges.binary_search_by(|r| {
            if r.end <= start {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Greater
            }
        }) {
            Ok(idx) | Err(idx) => {
                if idx >= self.ranges.len() {
                    return false;
                }
                let r = self.ranges[idx];
                r.start <= start && r.end >= end
            }
        }
    }

    /// Insert the given range, merging overlaps/adjacent ranges.
    pub fn insert(&mut self, start: u64, end: u64) {
        let mut new = ByteRange::new(start, end);
        if new.is_empty() {
            return;
        }

        let mut out = Vec::with_capacity(self.ranges.len() + 1);
        let mut inserted = false;

        for r in self.ranges.drain(..) {
            if r.end < new.start {
                out.push(r);
                continue;
            }
            if new.end < r.start {
                if !inserted {
                    out.push(new);
                    inserted = true;
                }
                out.push(r);
                continue;
            }
            // Overlapping or adjacent.
            new = new.merge(&r);
        }

        if !inserted {
            out.push(new);
        }

        self.ranges = out;
        self.compact_adjacent();
    }

    /// Remove the given range from the set.
    pub fn remove(&mut self, start: u64, end: u64) {
        let remove = ByteRange::new(start, end);
        if remove.is_empty() {
            return;
        }

        let mut out = Vec::with_capacity(self.ranges.len());
        for r in self.ranges.drain(..) {
            // No overlap
            if r.end <= remove.start || r.start >= remove.end {
                out.push(r);
                continue;
            }

            // Left remainder
            if r.start < remove.start {
                out.push(ByteRange::new(r.start, remove.start));
            }

            // Right remainder
            if r.end > remove.end {
                out.push(ByteRange::new(remove.end, r.end));
            }
        }
        self.ranges = out;
        self.compact_adjacent();
    }

    fn compact_adjacent(&mut self) {
        if self.ranges.len() <= 1 {
            return;
        }

        self.ranges.sort_by_key(|r| r.start);

        let mut compacted = Vec::with_capacity(self.ranges.len());
        let mut cur = self.ranges[0];
        for r in self.ranges.iter().skip(1).copied() {
            if cur.overlaps_or_adjacent(&r) {
                cur = cur.merge(&r);
            } else {
                compacted.push(cur);
                cur = r;
            }
        }
        compacted.push(cur);
        self.ranges = compacted;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_merges_overlaps_and_adjacency() {
        let mut rs = RangeSet::new();
        rs.insert(10, 20);
        rs.insert(0, 5);
        rs.insert(5, 10); // adjacent
        rs.insert(18, 25); // overlaps

        assert_eq!(rs.ranges(), &[ByteRange::new(0, 25)]);
    }

    #[test]
    fn remove_splits_ranges() {
        let mut rs = RangeSet::new();
        rs.insert(0, 100);
        rs.remove(25, 75);

        assert_eq!(
            rs.ranges(),
            &[ByteRange::new(0, 25), ByteRange::new(75, 100)]
        );
    }

    #[test]
    fn contains_range_works() {
        let mut rs = RangeSet::new();
        rs.insert(0, 10);
        rs.insert(20, 30);

        assert!(rs.contains_range(0, 1));
        assert!(rs.contains_range(1, 10));
        assert!(!rs.contains_range(9, 11));
        assert!(!rs.contains_range(10, 20));
        assert!(rs.contains_range(20, 30));
    }
}
