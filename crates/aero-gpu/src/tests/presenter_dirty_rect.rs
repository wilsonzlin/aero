use crate::{Presenter, Rect, TextureWriter};

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

    fn fill_bytes(&mut self, out: &mut [u8]) {
        for b in out {
            *b = (self.next_u32() >> 24) as u8;
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WriteCall {
    rect: Rect,
    bytes_per_row: usize,
    data: Vec<u8>,
}

#[derive(Debug, Default)]
struct RecordingWriter {
    calls: Vec<WriteCall>,
}

impl TextureWriter for RecordingWriter {
    fn write_texture(&mut self, rect: Rect, bytes_per_row: usize, data: &[u8]) {
        self.calls.push(WriteCall {
            rect,
            bytes_per_row,
            data: data.to_vec(),
        });
    }
}

fn align_up(val: usize, align: usize) -> usize {
    debug_assert!(align.is_power_of_two());
    val.checked_add(align - 1)
        .map(|v| v & !(align - 1))
        .unwrap_or(usize::MAX)
}

fn required_data_len(bytes_per_row: usize, row_bytes: usize, copy_height: usize) -> usize {
    if copy_height == 0 {
        return 0;
    }
    bytes_per_row
        .saturating_mul(copy_height.saturating_sub(1))
        .saturating_add(row_bytes)
}

fn min_frame_len(stride: usize, height: usize, row_bytes: usize) -> usize {
    if height == 0 {
        return 0;
    }
    stride
        .saturating_mul(height.saturating_sub(1))
        .saturating_add(row_bytes)
}

fn assert_rect_within_bounds(rect: Rect, width: u32, height: u32) {
    assert!(
        u64::from(rect.x) + u64::from(rect.w) <= u64::from(width),
        "rect exceeds width {width}: {rect:?}"
    );
    assert!(
        u64::from(rect.y) + u64::from(rect.h) <= u64::from(height),
        "rect exceeds height {height}: {rect:?}"
    );
}

#[test]
fn presenter_randomized_uploads_match_source_slices() {
    // End-to-end property test for dirty-rect handling inside `Presenter::present`.
    //
    // This intentionally feeds in-bounds and out-of-bounds rectangles and validates that:
    // - All texture write rects are clamped to framebuffer bounds.
    // - The upload data matches the source framebuffer bytes for each rect row.
    // - Data lengths match the wgpu-required layout (bytes_per_row/required_len).
    //
    // These invariants prevent regressions that could cause out-of-bounds texture uploads or
    // incorrect row slicing/copying.
    const COPY_BYTES_PER_ROW_ALIGNMENT: usize = 256;

    let mut rng = Rng::new(0xA11C_E5ED_5EED_C0DE);

    for _case in 0..250 {
        let width = rng.gen_range_u32(1..=128);
        let height = rng.gen_range_u32(1..=128);

        let bpp = match rng.gen_range_u32(0..=2) {
            0 => 1,
            1 => 2,
            _ => 4,
        };

        let row_bytes = width as usize * bpp;
        let stride = row_bytes + rng.gen_range_usize(0..=64);
        let frame_len = min_frame_len(stride, height as usize, row_bytes);

        let mut frame_data = vec![0u8; frame_len];
        rng.fill_bytes(&mut frame_data);

        let cap = rng.gen_range_usize(0..=16);
        let mut presenter = Presenter::new(width, height, bpp, RecordingWriter::default())
            .with_max_rects_per_frame(cap);

        let rect_count = rng.gen_range_usize(0..=32);
        let mut dirty = Vec::with_capacity(rect_count);
        for _ in 0..rect_count {
            // Intentionally allow some out-of-bounds values to exercise clamping/dropping.
            let x = rng.gen_range_u32(0..=width + 64);
            let y = rng.gen_range_u32(0..=height + 64);
            let w = rng.gen_range_u32(0..=width + 64);
            let h = rng.gen_range_u32(0..=height + 64);
            dirty.push(Rect::new(x, y, w, h));
        }

        let telemetry = presenter
            .present(&frame_data, stride, Some(&dirty))
            .expect("present should succeed for well-formed frame_data/stride");

        assert_eq!(telemetry.rects_requested, dirty.len());
        assert_eq!(presenter.writer().calls.len(), telemetry.rects_uploaded);
        assert!(telemetry.rects_uploaded <= cap);

        if cap == 0 {
            assert!(presenter.writer().calls.is_empty());
            assert_eq!(telemetry.bytes_uploaded, 0);
            continue;
        }

        let sum_uploaded: usize = presenter.writer().calls.iter().map(|c| c.data.len()).sum();
        assert_eq!(telemetry.bytes_uploaded, sum_uploaded);

        for call in &presenter.writer().calls {
            let rect = call.rect;
            assert_rect_within_bounds(rect, width, height);
            assert!(rect.w > 0 && rect.h > 0);

            let row_bytes_rect = rect.w as usize * bpp;
            let expected_bpr = if rect.h as usize <= 1 {
                row_bytes_rect
            } else {
                align_up(row_bytes_rect, COPY_BYTES_PER_ROW_ALIGNMENT)
            };
            assert_eq!(call.bytes_per_row, expected_bpr);

            let expected_len = required_data_len(expected_bpr, row_bytes_rect, rect.h as usize);
            assert_eq!(call.data.len(), expected_len);

            // Validate row copies for the active row_bytes range (padding bytes are don't-care).
            for row in 0..rect.h as usize {
                let src_off = (rect.y as usize + row) * stride + rect.x as usize * bpp;
                let dst_off = row * expected_bpr;

                assert_eq!(
                    &call.data[dst_off..dst_off + row_bytes_rect],
                    &frame_data[src_off..src_off + row_bytes_rect],
                    "row mismatch: rect={rect:?} row={row} stride={stride} bpp={bpp}"
                );
            }
        }
    }
}
