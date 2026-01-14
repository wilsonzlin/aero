use crate::{merge_and_cap_rects, Rect};

/// Simple deterministic PRNG (xorshift64*) for "property-like" tests without external deps.
///
/// We avoid pulling in `rand`/`proptest` and keep the sequence stable across platforms.
#[derive(Clone)]
struct Rng(u64);

impl Rng {
    const fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next_u64(&mut self) -> u64 {
        // xorshift64*
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn next_u32(&mut self) -> u32 {
        (self.next_u64() >> 32) as u32
    }

    fn gen_range_u32(&mut self, range: std::ops::RangeInclusive<u32>) -> u32 {
        let start = *range.start();
        let end = *range.end();
        if start == end {
            return start;
        }
        let span = end.wrapping_sub(start).wrapping_add(1);
        // Bias is fine for tests; keep it simple and deterministic.
        start.wrapping_add(self.next_u32() % span)
    }

    fn gen_range_usize(&mut self, range: std::ops::RangeInclusive<usize>) -> usize {
        let start = *range.start();
        let end = *range.end();
        if start == end {
            return start;
        }
        let span = end - start + 1;
        start + (self.next_u64() as usize % span)
    }
}

fn assert_rect_within_bounds(rect: Rect, width: u32, height: u32) {
    assert!(
        rect.w > 0 && rect.h > 0,
        "rect should be non-empty: {rect:?}"
    );
    assert!(
        rect.x < width && rect.y < height,
        "rect origin out of bounds (width={width}, height={height}): {rect:?}"
    );
    assert!(
        u64::from(rect.x) + u64::from(rect.w) <= u64::from(width),
        "rect extends past width (width={width}): {rect:?}"
    );
    assert!(
        u64::from(rect.y) + u64::from(rect.h) <= u64::from(height),
        "rect extends past height (height={height}): {rect:?}"
    );
}

fn rect_contains(outer: Rect, inner: Rect) -> bool {
    u64::from(outer.x) <= u64::from(inner.x)
        && u64::from(outer.y) <= u64::from(inner.y)
        && u64::from(outer.x) + u64::from(outer.w) >= u64::from(inner.x) + u64::from(inner.w)
        && u64::from(outer.y) + u64::from(outer.h) >= u64::from(inner.y) + u64::from(inner.h)
}

#[test]
fn merge_and_cap_rects_empty_input_is_a_noop() {
    // Canonical policy: the absence of dirty rects is treated as "no update", not "full frame".
    // This allows callers to explicitly skip uploads for frames that are known to be unchanged.
    let out = merge_and_cap_rects(&[], (64, 64), 128);
    assert!(out.rects.is_empty());
    assert_eq!(out.rects_clamped, 0);
    assert_eq!(out.rects_after_merge, 0);
    assert_eq!(out.rects_after_cap, 0);
}

#[test]
fn merge_and_cap_rects_clamps_to_framebuffer_bounds() {
    let bounds = (10u32, 10u32);
    let rects = [
        Rect::new(8, 8, 10, 10), // partially out-of-bounds -> clamps to 2x2.
        Rect::new(20, 0, 5, 5),  // fully out-of-bounds -> dropped.
        Rect::new(0, 0, 1, 1),   // in-bounds.
    ];

    let out = merge_and_cap_rects(&rects, bounds, 128);
    assert_eq!(
        out.rects,
        vec![Rect::new(0, 0, 1, 1), Rect::new(8, 8, 2, 2)]
    );
    for r in &out.rects {
        assert_rect_within_bounds(*r, bounds.0, bounds.1);
    }
}

#[test]
fn rect_clamp_to_bounds_saturates_on_overflow() {
    // Ensure `right()`/`bottom()` use saturating arithmetic so huge extents clamp correctly instead
    // of wrapping and causing the rect to be dropped.
    let r = Rect::new(5, 5, u32::MAX, u32::MAX);
    assert_eq!(r.right(), u32::MAX);
    assert_eq!(r.bottom(), u32::MAX);
    assert_eq!(r.clamp_to_bounds(10, 10), Some(Rect::new(5, 5, 5, 5)));
}

#[test]
fn merge_and_cap_rects_merges_transitively() {
    // r1 adjacent to r2, r2 adjacent to r3 => all should merge into one bbox.
    let r1 = Rect::new(0, 0, 10, 10);
    let r2 = Rect::new(10, 0, 10, 10);
    let r3 = Rect::new(20, 0, 10, 10);

    let out = merge_and_cap_rects(&[r1, r2, r3], (100, 100), 128);
    assert_eq!(out.rects, vec![Rect::new(0, 0, 30, 10)]);
}

#[test]
fn merge_and_cap_rects_merges_edge_adjacent_rects() {
    let r1 = Rect::new(0, 0, 10, 10);
    let r2 = Rect::new(10, 2, 5, 5);

    let out = merge_and_cap_rects(&[r1, r2], (100, 100), 128);
    assert_eq!(out.rects, vec![Rect::new(0, 0, 15, 10)]);
}

#[test]
fn merge_and_cap_rects_does_not_merge_corner_touching_rects() {
    let r1 = Rect::new(0, 0, 10, 10);
    let r2 = Rect::new(10, 10, 5, 5);

    let out = merge_and_cap_rects(&[r1, r2], (100, 100), 128);
    assert_eq!(out.rects, vec![r1, r2]);
}

#[test]
fn merge_and_cap_rects_cap_one_returns_bounding_box() {
    let bounds = (100u32, 100u32);
    let rects = [
        Rect::new(10, 10, 5, 5),
        Rect::new(50, 50, 10, 10),
        Rect::new(90, 90, 20, 20), // clamps to reach the bottom-right corner.
    ];

    let out = merge_and_cap_rects(&rects, bounds, 1);
    assert_eq!(out.rects, vec![Rect::new(10, 10, 90, 90)]);
}

#[test]
fn merge_and_cap_rects_randomized_invariants() {
    // This test is specifically aimed at catching regressions that could lead to out-of-bounds
    // texture uploads in the presenter (e.g. `x+w > width`).
    let mut rng = Rng::new(0xC0FFEE_FACADE_1234);

    for _case in 0..2_000 {
        let width = rng.gen_range_u32(0..=512);
        let height = rng.gen_range_u32(0..=512);
        let cap = rng.gen_range_usize(0..=64);

        let rect_count = rng.gen_range_usize(0..=128);
        let mut rects = Vec::with_capacity(rect_count);
        for _ in 0..rect_count {
            rects.push(Rect::new(
                rng.next_u32(),
                rng.next_u32(),
                rng.next_u32(),
                rng.next_u32(),
            ));
        }

        let out = merge_and_cap_rects(&rects, (width, height), cap);

        // Internal consistency.
        assert_eq!(out.rects_after_cap, out.rects.len());

        if width == 0 || height == 0 || cap == 0 {
            assert!(
                out.rects.is_empty(),
                "expected empty output for bounds=({width},{height}) cap={cap}, got {out:?}"
            );
            continue;
        }

        assert!(
            out.rects.len() <= cap,
            "output rect list exceeds cap={cap}: {}",
            out.rects.len()
        );

        for r in &out.rects {
            assert_rect_within_bounds(*r, width, height);
        }

        // Coverage invariant: every in-bounds input rect must be fully covered by at least one
        // output rect (since merge/cap only unions rectangles; it should never "drop" coverage).
        let mut clamped_inputs = Vec::new();
        for &r in &rects {
            if let Some(r) = r.clamp_to_bounds(width, height) {
                if !r.is_empty() {
                    clamped_inputs.push(r);
                }
            }
        }
        for in_r in clamped_inputs {
            assert!(
                out.rects.iter().copied().any(|out_r| rect_contains(out_r, in_r)),
                "input rect not covered by outputs (bounds=({width},{height}) cap={cap}): input={in_r:?} outputs={:?}",
                out.rects
            );
        }
    }
}
