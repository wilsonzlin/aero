use std::cmp::{max, min};

/// Dirty rectangle in framebuffer coordinates.
///
/// The rectangle is expressed as an origin `(x, y)` with extent `(w, h)`.
/// `w == 0` or `h == 0` represents an empty rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    #[must_use]
    pub const fn new(x: u32, y: u32, w: u32, h: u32) -> Self {
        Self { x, y, w, h }
    }

    #[must_use]
    pub fn right(self) -> u32 {
        self.x.saturating_add(self.w)
    }

    #[must_use]
    pub fn bottom(self) -> u32 {
        self.y.saturating_add(self.h)
    }

    #[must_use]
    pub fn is_empty(self) -> bool {
        self.w == 0 || self.h == 0
    }

    #[must_use]
    pub fn area(self) -> u64 {
        u64::from(self.w) * u64::from(self.h)
    }

    /// Clamp the rect to the `[0,width)×[0,height)` bounds.
    #[must_use]
    pub fn clamp_to_bounds(self, width: u32, height: u32) -> Option<Self> {
        if width == 0 || height == 0 {
            return None;
        }

        let x0 = min(self.x, width);
        let y0 = min(self.y, height);
        let x1 = min(self.right(), width);
        let y1 = min(self.bottom(), height);

        if x1 <= x0 || y1 <= y0 {
            None
        } else {
            Some(Self::new(x0, y0, x1 - x0, y1 - y0))
        }
    }

    #[must_use]
    pub fn union(self, other: Self) -> Self {
        let x0 = min(self.x, other.x);
        let y0 = min(self.y, other.y);
        let x1 = max(self.right(), other.right());
        let y1 = max(self.bottom(), other.bottom());
        Self::new(x0, y0, x1 - x0, y1 - y0)
    }

    #[must_use]
    pub fn overlaps(self, other: Self) -> bool {
        self.x < other.right()
            && self.right() > other.x
            && self.y < other.bottom()
            && self.bottom() > other.y
    }

    /// Returns `true` if the two rectangles overlap, or share an edge (horizontal or vertical).
    ///
    /// Corner-touching rectangles are *not* considered adjacent, because merging them tends to
    /// create very large bounding boxes for little benefit.
    #[must_use]
    pub fn overlaps_or_adjacent(self, other: Self) -> bool {
        if self.overlaps(other) {
            return true;
        }

        let horizontal_overlap = self.x < other.right() && self.right() > other.x;
        let vertical_overlap = self.y < other.bottom() && self.bottom() > other.y;

        // Share a vertical edge with some vertical overlap.
        let horizontal_adjacent =
            vertical_overlap && (self.right() == other.x || other.right() == self.x);

        // Share a horizontal edge with some horizontal overlap.
        let vertical_adjacent =
            horizontal_overlap && (self.bottom() == other.y || other.bottom() == self.y);

        horizontal_adjacent || vertical_adjacent
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RectMergeOutcome {
    pub rects: Vec<Rect>,
    pub rects_clamped: usize,
    pub rects_after_merge: usize,
    pub rects_after_cap: usize,
}

// The merge algorithm for small rect lists uses an O(n^2) overlap/adjacency check. Keep it robust
// against guest-controlled pathological inputs by bounding both the number of rects we will merge
// exactly and the total number of rects we'll even attempt to process before falling back to a
// conservative full-frame update.
const MAX_INPUT_RECTS: usize = 1_000_000;
const MAX_CLAMPED_RECTS: usize = 65_536;
const MAX_EXACT_MERGE_RECTS: usize = 4096;

/// Merge overlapping/adjacent rectangles and cap the output list length.
///
/// - Overlapping or edge-adjacent rects are merged into their bounding box.
/// - If the resulting list exceeds `cap`, rects are merged into larger bounding boxes until
///   `len <= cap`. This is done by sorting and grouping deterministically.
#[must_use]
pub fn merge_and_cap_rects(rects: &[Rect], bounds: (u32, u32), cap: usize) -> RectMergeOutcome {
    let (width, height) = bounds;

    if width == 0 || height == 0 {
        return RectMergeOutcome {
            rects: Vec::new(),
            rects_clamped: 0,
            rects_after_merge: 0,
            rects_after_cap: 0,
        };
    }

    let full_frame = || {
        let rects_after_merge = 1usize;
        let rects = if cap == 0 {
            Vec::new()
        } else {
            vec![Rect::new(0, 0, width, height)]
        };
        RectMergeOutcome {
            rects_clamped: 0,
            rects_after_merge,
            rects_after_cap: rects.len(),
            rects,
        }
    };

    if rects.len() > MAX_INPUT_RECTS {
        return full_frame();
    }

    let mut clamped = Vec::new();
    if clamped
        .try_reserve_exact(rects.len().min(MAX_CLAMPED_RECTS))
        .is_err()
    {
        return full_frame();
    }
    for &rect in rects {
        if let Some(rect) = rect.clamp_to_bounds(width, height) {
            if !rect.is_empty() {
                if clamped.len() >= MAX_CLAMPED_RECTS {
                    return full_frame();
                }
                clamped.push(rect);
            }
        }
    }
    let rects_clamped = clamped.len();

    let mut merged = if clamped.len() > MAX_EXACT_MERGE_RECTS {
        // Large inputs can be guest-controlled (e.g. dirty-tile bitmaps with tiny tile sizes). Skip
        // the quadratic overlap/adjacency merge and rely on the deterministic `cap` grouping
        // instead, which is O(n log n) due to sorting.
        clamped
    } else {
        merge_overlapping_and_adjacent(&clamped)
    };
    let rects_after_merge = merged.len();

    cap_rects_in_place(&mut merged, cap);
    let rects_after_cap = merged.len();

    RectMergeOutcome {
        rects: merged,
        rects_clamped,
        rects_after_merge,
        rects_after_cap,
    }
}

#[must_use]
fn merge_overlapping_and_adjacent(rects: &[Rect]) -> Vec<Rect> {
    if rects.is_empty() {
        return Vec::new();
    }

    // Union-find on the overlap/adjacency graph.
    let mut uf = UnionFind::new(rects.len());
    for i in 0..rects.len() {
        for j in (i + 1)..rects.len() {
            if rects[i].overlaps_or_adjacent(rects[j]) {
                uf.union(i, j);
            }
        }
    }

    let mut bbox_by_root: std::collections::HashMap<usize, Rect> = std::collections::HashMap::new();
    for (idx, &rect) in rects.iter().enumerate() {
        let root = uf.find(idx);
        bbox_by_root
            .entry(root)
            .and_modify(|bbox| *bbox = bbox.union(rect))
            .or_insert(rect);
    }

    let mut out: Vec<Rect> = bbox_by_root.into_values().collect();
    // Deterministic output order.
    out.sort_by_key(|r| (r.y, r.x, r.h, r.w));
    out
}

fn cap_rects_in_place(rects: &mut Vec<Rect>, cap: usize) {
    if cap == 0 {
        rects.clear();
        return;
    }
    if rects.len() <= cap {
        return;
    }

    // Sort to make grouping deterministic and spatially coherent-ish.
    rects.sort_by_key(|r| (r.y, r.x));
    let group_size = rects.len().div_ceil(cap);

    let mut capped = Vec::with_capacity(cap);
    for chunk in rects.chunks(group_size) {
        let mut bbox = chunk[0];
        for &rect in &chunk[1..] {
            bbox = bbox.union(rect);
        }
        capped.push(bbox);
    }
    *rects = capped;
}

#[derive(Debug)]
struct UnionFind {
    parent: Vec<usize>,
    rank: Vec<u8>,
}

impl UnionFind {
    fn new(n: usize) -> Self {
        Self {
            parent: (0..n).collect(),
            rank: vec![0; n],
        }
    }

    fn find(&mut self, x: usize) -> usize {
        if self.parent[x] != x {
            let root = self.find(self.parent[x]);
            self.parent[x] = root;
        }
        self.parent[x]
    }

    fn union(&mut self, a: usize, b: usize) {
        let mut ra = self.find(a);
        let mut rb = self.find(b);
        if ra == rb {
            return;
        }

        if self.rank[ra] < self.rank[rb] {
            std::mem::swap(&mut ra, &mut rb);
        }
        self.parent[rb] = ra;
        if self.rank[ra] == self.rank[rb] {
            self.rank[ra] = self.rank[ra].saturating_add(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn merges_overlapping_rects() {
        let r1 = Rect::new(0, 0, 10, 10);
        let r2 = Rect::new(5, 5, 10, 10);

        let out = merge_and_cap_rects(&[r1, r2], (100, 100), 128);
        assert_eq!(out.rects, vec![Rect::new(0, 0, 15, 15)]);
        assert_eq!(out.rects_clamped, 2);
        assert_eq!(out.rects_after_merge, 1);
        assert_eq!(out.rects_after_cap, 1);
    }

    #[test]
    fn merges_edge_adjacent_rects() {
        let r1 = Rect::new(0, 0, 10, 10);
        let r2 = Rect::new(10, 2, 5, 5);

        let out = merge_and_cap_rects(&[r1, r2], (100, 100), 128);
        assert_eq!(out.rects, vec![Rect::new(0, 0, 15, 10)]);
    }

    #[test]
    fn does_not_merge_corner_touching_rects() {
        let r1 = Rect::new(0, 0, 10, 10);
        let r2 = Rect::new(10, 10, 5, 5);

        let out = merge_and_cap_rects(&[r1, r2], (100, 100), 128);
        assert_eq!(out.rects.len(), 2);
    }

    #[test]
    fn caps_rect_count_by_grouping() {
        let rects: Vec<_> = (0..9).map(|i| Rect::new(i * 2, 0, 1, 1)).collect();
        let out = merge_and_cap_rects(&rects, (100, 100), 4);
        assert!(out.rects.len() <= 4);

        // Grouping uses a group size of ceil(9/4)=3 -> 3 output rects.
        assert_eq!(out.rects.len(), 3);
    }

    #[test]
    fn large_rect_lists_skip_quadratic_merge_and_still_cap() {
        // This hits the "large input" fallback path (skip O(n^2) merge) without requiring a huge
        // allocation.
        let rects: Vec<_> = (0..(MAX_EXACT_MERGE_RECTS + 1))
            .map(|i| Rect::new((i % 256) as u32, (i / 256) as u32, 1, 1))
            .collect();
        let out = merge_and_cap_rects(&rects, (1024, 1024), 10);
        assert_eq!(out.rects_after_merge, rects.len());
        assert_eq!(out.rects.len(), 10);
    }

    #[test]
    fn too_many_rects_fall_back_to_full_frame() {
        let rects: Vec<_> = (0..(MAX_CLAMPED_RECTS + 1))
            .map(|i| Rect::new((i % 256) as u32, (i / 256) as u32, 1, 1))
            .collect();
        let out = merge_and_cap_rects(&rects, (100, 100), 128);
        assert_eq!(out.rects, vec![Rect::new(0, 0, 100, 100)]);
    }
}
