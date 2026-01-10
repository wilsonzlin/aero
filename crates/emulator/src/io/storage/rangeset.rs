use serde::{Deserialize, Serialize};

/// A set of half-open integer ranges `[start, end)` stored as a sorted, disjoint
/// list.
///
/// Adjacent ranges (e.g. `[0, 10)` and `[10, 20)`) are automatically coalesced.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RangeSet {
    ranges: Vec<Range>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Range {
    pub start: u64,
    pub end: u64,
}

impl Range {
    #[inline]
    pub fn len(self) -> u64 {
        self.end - self.start
    }

    #[inline]
    pub fn is_empty(self) -> bool {
        self.start >= self.end
    }
}

impl RangeSet {
    pub fn new() -> Self {
        Self { ranges: Vec::new() }
    }

    pub fn iter(&self) -> impl Iterator<Item = Range> + '_ {
        self.ranges.iter().copied()
    }

    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }

    /// Inserts `[start, end)` into the set, merging any overlapping/adjacent
    /// ranges.
    pub fn insert(&mut self, start: u64, end: u64) {
        if start >= end {
            return;
        }

        let mut new = Range { start, end };
        let mut i = 0;
        while i < self.ranges.len() {
            let cur = self.ranges[i];

            // Existing range is fully before the new one.
            if cur.end < new.start {
                i += 1;
                continue;
            }

            // Existing range is fully after the new one.
            if new.end < cur.start {
                break;
            }

            // Overlapping or adjacent: merge and remove current.
            new.start = new.start.min(cur.start);
            new.end = new.end.max(cur.end);
            self.ranges.remove(i);
        }

        self.ranges.insert(i, new);
    }

    /// Removes `[start, end)` from the set, splitting ranges as required.
    pub fn remove(&mut self, start: u64, end: u64) {
        if start >= end {
            return;
        }

        let mut i = 0;
        while i < self.ranges.len() {
            let cur = self.ranges[i];
            if cur.end <= start {
                i += 1;
                continue;
            }
            if cur.start >= end {
                break;
            }

            // Overlap exists. There are up to two remaining pieces: left and/or right.
            let left = Range {
                start: cur.start,
                end: cur.end.min(start),
            };
            let right = Range {
                start: cur.start.max(end),
                end: cur.end,
            };

            // Replace current with the remaining pieces (if any).
            self.ranges.remove(i);
            if !right.is_empty() {
                self.ranges.insert(i, right);
            }
            if !left.is_empty() {
                self.ranges.insert(i, left);
                i += 1;
            }
        }
    }

    /// Returns true if every byte in `[start, end)` is contained by the set.
    pub fn contains_range(&self, start: u64, end: u64) -> bool {
        if start >= end {
            return true;
        }

        for r in &self.ranges {
            if r.end <= start {
                continue;
            }
            return r.start <= start && r.end >= end;
        }

        false
    }

    /// Returns the list of missing ranges inside `[start, end)` that are not
    /// covered by this set.
    pub fn gaps(&self, start: u64, end: u64) -> Vec<Range> {
        if start >= end {
            return Vec::new();
        }

        let mut out = Vec::new();
        let mut cursor = start;

        for r in &self.ranges {
            if r.end <= cursor {
                continue;
            }
            if r.start >= end {
                break;
            }

            if r.start > cursor {
                out.push(Range {
                    start: cursor,
                    end: r.start.min(end),
                });
            }

            cursor = cursor.max(r.end);
            if cursor >= end {
                break;
            }
        }

        if cursor < end {
            out.push(Range { start: cursor, end });
        }

        out
    }

    /// Returns the list of ranges in this set that intersect `[start, end)`.
    pub fn intersecting(&self, start: u64, end: u64) -> Vec<Range> {
        if start >= end {
            return Vec::new();
        }

        let mut out = Vec::new();
        for r in &self.ranges {
            if r.end <= start {
                continue;
            }
            if r.start >= end {
                break;
            }
            out.push(Range {
                start: r.start.max(start),
                end: r.end.min(end),
            });
        }
        out
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
        rs.insert(5, 10); // adjacent => merge
        rs.insert(18, 30); // overlap => merge

        assert_eq!(
            rs.iter().collect::<Vec<_>>(),
            vec![Range { start: 0, end: 30 }]
        );
    }

    #[test]
    fn gaps_returns_missing_segments() {
        let mut rs = RangeSet::new();
        rs.insert(10, 20);
        rs.insert(30, 40);

        assert_eq!(
            rs.gaps(0, 50),
            vec![
                Range { start: 0, end: 10 },
                Range { start: 20, end: 30 },
                Range { start: 40, end: 50 }
            ]
        );
    }

    #[test]
    fn remove_splits_ranges() {
        let mut rs = RangeSet::new();
        rs.insert(0, 100);
        rs.remove(25, 75);

        assert_eq!(
            rs.iter().collect::<Vec<_>>(),
            vec![Range { start: 0, end: 25 }, Range { start: 75, end: 100 }]
        );
    }

    #[test]
    fn contains_range_checks_full_coverage() {
        let mut rs = RangeSet::new();
        rs.insert(0, 10);
        rs.insert(20, 30);

        assert!(rs.contains_range(2, 9));
        assert!(!rs.contains_range(2, 12));
        assert!(!rs.contains_range(10, 20));
    }
}
