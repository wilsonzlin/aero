use crate::tile_diff::TileDiff;
use crate::Rect;

const TILE_SIZE: u32 = 32;

/// Deterministic PRNG for randomized tests without bringing in `rand`/`proptest`.
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
        let span = end - start + 1;
        start + (self.next_u32() % span)
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

fn pack_frame(frame: &[u8], stride: usize, row_bytes: usize, height: u32) -> Vec<u8> {
    let packed_len = row_bytes * height as usize;
    let mut out = vec![0u8; packed_len];
    for row in 0..height as usize {
        let src_off = row * stride;
        let dst_off = row * row_bytes;
        out[dst_off..dst_off + row_bytes].copy_from_slice(&frame[src_off..src_off + row_bytes]);
    }
    out
}

fn expected_dirty_tiles(
    prev_packed: &[u8],
    frame: &[u8],
    stride: usize,
    width: u32,
    height: u32,
    bpp: usize,
) -> Vec<Rect> {
    let row_bytes = width as usize * bpp;
    let packed_row_bytes = row_bytes;

    let tiles_x = width.div_ceil(TILE_SIZE);
    let tiles_y = height.div_ceil(TILE_SIZE);

    let mut dirty = Vec::new();
    for ty in 0..tiles_y {
        for tx in 0..tiles_x {
            let x = tx * TILE_SIZE;
            let y = ty * TILE_SIZE;
            let w = (width - x).min(TILE_SIZE);
            let h = (height - y).min(TILE_SIZE);

            let row_len = w as usize * bpp;
            let mut differs = false;
            for row in 0..h as usize {
                let cur_off = (y as usize + row) * stride + x as usize * bpp;
                let prev_off = (y as usize + row) * packed_row_bytes + x as usize * bpp;
                if frame[cur_off..cur_off + row_len] != prev_packed[prev_off..prev_off + row_len] {
                    differs = true;
                    break;
                }
            }

            if differs {
                dirty.push(Rect::new(x, y, w, h));
            }
        }
    }
    dirty
}

fn assert_tile_rect_invariants(rect: Rect, width: u32, height: u32) {
    assert!(
        rect.x < width && rect.y < height,
        "rect origin out of bounds for {width}x{height}: {rect:?}"
    );
    assert!(
        u64::from(rect.x) + u64::from(rect.w) <= u64::from(width),
        "rect extends past width for {width}x{height}: {rect:?}"
    );
    assert!(
        u64::from(rect.y) + u64::from(rect.h) <= u64::from(height),
        "rect extends past height for {width}x{height}: {rect:?}"
    );

    assert!(
        rect.x % TILE_SIZE == 0 && rect.y % TILE_SIZE == 0,
        "tile rect must start on a {TILE_SIZE}x{TILE_SIZE} grid: {rect:?}"
    );
    assert!(rect.w > 0 && rect.h > 0);
    assert!(rect.w <= TILE_SIZE && rect.h <= TILE_SIZE);
}

#[test]
fn tile_diff_noop_second_frame_returns_no_rects() {
    let (width, height, bpp) = (64u32, 48u32, 4usize);
    let row_bytes = width as usize * bpp;
    let stride = row_bytes;
    let frame = vec![0xABu8; stride * height as usize];

    let mut diff = TileDiff::new(width, height, bpp);
    assert_eq!(diff.diff(&frame, stride), vec![Rect::new(0, 0, width, height)]);
    assert_eq!(diff.diff(&frame, stride), Vec::<Rect>::new());
}

#[test]
fn tile_diff_single_pixel_change_marks_containing_tile() {
    let (width, height, bpp) = (50u32, 50u32, 4usize);
    let row_bytes = width as usize * bpp;
    let stride = row_bytes;

    let mut frame0 = vec![0u8; stride * height as usize];
    let mut diff = TileDiff::new(width, height, bpp);
    diff.diff(&frame0, stride);

    // Change a byte in the bottom-right corner (last partial tile).
    let x = 49usize;
    let y = 49usize;
    frame0[y * stride + x * bpp] ^= 0xFF;

    let dirty = diff.diff(&frame0, stride);
    assert_eq!(dirty, vec![Rect::new(32, 32, 18, 18)]);
}

#[test]
fn tile_diff_ignores_stride_padding_bytes() {
    // If the CPU framebuffer has a padded stride, changes to the padding bytes must *not* mark the
    // tile dirty (presenter uploads only operate on width*bpp bytes per row).
    let (width, height, bpp) = (5u32, 5u32, 4usize);
    let row_bytes = width as usize * bpp;
    let stride = row_bytes + 7;

    let mut frame = vec![0u8; stride * height as usize];
    for (i, b) in frame.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(17);
    }

    let mut diff = TileDiff::new(width, height, bpp);
    diff.diff(&frame, stride);

    // Mutate only padding bytes.
    for row in 0..height as usize {
        let base = row * stride + row_bytes;
        for b in &mut frame[base..base + (stride - row_bytes)] {
            *b ^= 0xFF;
        }
    }

    let dirty = diff.diff(&frame, stride);
    assert!(dirty.is_empty(), "expected no dirty tiles; got {dirty:?}");
}

#[test]
fn tile_diff_stride_too_small_falls_back_to_full_frame() {
    let (width, height, bpp) = (8u32, 8u32, 4usize);
    let row_bytes = width as usize * bpp;
    let stride = row_bytes - 1; // malformed

    let frame = vec![0x11u8; stride * height as usize];
    let mut diff = TileDiff::new(width, height, bpp);

    let dirty = diff.diff(&frame, stride);
    assert_eq!(dirty, vec![Rect::new(0, 0, width, height)]);
}

#[test]
fn tile_diff_randomized_matches_reference_and_stays_in_bounds() {
    let mut rng = Rng::new(0xD1FF_D1FF_5EED_5EED);

    for _case in 0..300 {
        let width = rng.gen_range_u32(1..=160);
        let height = rng.gen_range_u32(1..=160);
        let bpp = match rng.gen_range_u32(0..=2) {
            0 => 1,
            1 => 2,
            _ => 4,
        };

        let row_bytes = width as usize * bpp;
        let padding = rng.gen_range_usize(0..=64);
        let stride = row_bytes + padding;

        // Generate a random initial frame (including padding bytes).
        let mut frame = vec![0u8; stride * height as usize];
        for b in &mut frame {
            *b = rng.next_u32() as u8;
        }

        let mut diff = TileDiff::new(width, height, bpp);

        // First frame is always treated as fully dirty (no previous snapshot).
        let first = diff.diff(&frame, stride);
        assert_eq!(first, vec![Rect::new(0, 0, width, height)]);

        // Track our own "previous packed snapshot" as the reference implementation.
        let mut prev_packed = pack_frame(&frame, stride, row_bytes, height);

        // Apply a few random mutations and check that the returned dirty tiles match.
        for _step in 0..5 {
            let mut next = frame.clone();

            let changes = rng.gen_range_usize(0..=20);
            for _ in 0..changes {
                let x = rng.gen_range_usize(0..=width as usize - 1);
                let y = rng.gen_range_usize(0..=height as usize - 1);
                let chan = rng.gen_range_usize(0..=bpp - 1);
                let off = y * stride + x * bpp + chan;
                next[off] = next[off].wrapping_add(1);
            }

            let expected = expected_dirty_tiles(&prev_packed, &next, stride, width, height, bpp);
            let actual = diff.diff(&next, stride);
            assert_eq!(actual, expected);

            // Invariants: output tile rects must never exceed framebuffer bounds (prevents
            // out-of-bounds comparisons/uploads).
            for r in &actual {
                assert_tile_rect_invariants(*r, width, height);
            }

            // Prepare for the next iteration.
            prev_packed = pack_frame(&next, stride, row_bytes, height);
            frame = next;
        }
    }
}
