use crate::Rect;

const TILE_SIZE: u32 = 32;
const MAX_PREV_PACKED_BYTES: usize = 256 * 1024 * 1024;

/// Tile-based diff engine for generating dirty rectangles when the caller doesn't provide them.
///
/// This is intentionally behind a feature flag (`diff-engine`) and disabled by default, because
/// correctness/performance needs validation on real workloads and across browser implementations.
pub struct TileDiff {
    width: u32,
    height: u32,
    bytes_per_pixel: usize,
    prev_packed: Vec<u8>,
}

impl TileDiff {
    pub fn new(width: u32, height: u32, bytes_per_pixel: usize) -> Self {
        Self {
            width,
            height,
            bytes_per_pixel,
            prev_packed: Vec::new(),
        }
    }

    /// Returns dirty rectangles aligned to a `32×32` tile grid.
    pub fn diff(&mut self, frame_data: &[u8], stride: usize) -> Vec<Rect> {
        let Some(row_bytes) = (self.width as usize).checked_mul(self.bytes_per_pixel) else {
            // Avoid overflow and treat as full dirty. This is only used with trusted dimensions,
            // but keep it robust since it is driven by external configuration.
            self.prev_packed.clear();
            return vec![Rect::new(0, 0, self.width, self.height)];
        };
        let Some(packed_len) = row_bytes.checked_mul(self.height as usize) else {
            self.prev_packed.clear();
            return vec![Rect::new(0, 0, self.width, self.height)];
        };
        if packed_len > MAX_PREV_PACKED_BYTES {
            // Avoid pathological allocations for extremely large framebuffers.
            self.prev_packed.clear();
            return vec![Rect::new(0, 0, self.width, self.height)];
        }

        if stride < row_bytes {
            // Malformed input; fall back to full frame.
            self.snapshot(frame_data, stride, row_bytes, packed_len);
            return vec![Rect::new(0, 0, self.width, self.height)];
        }

        // If we don't have a previous frame (or size changed), treat as full dirty.
        if self.prev_packed.len() != packed_len {
            self.snapshot(frame_data, stride, row_bytes, packed_len);
            return vec![Rect::new(0, 0, self.width, self.height)];
        }

        let tiles_x = self.width.div_ceil(TILE_SIZE);
        let tiles_y = self.height.div_ceil(TILE_SIZE);

        let mut dirty = Vec::new();
        for ty in 0..tiles_y {
            for tx in 0..tiles_x {
                let x = tx * TILE_SIZE;
                let y = ty * TILE_SIZE;
                let w = (self.width - x).min(TILE_SIZE);
                let h = (self.height - y).min(TILE_SIZE);

                if self.tile_differs(frame_data, stride, x, y, w, h) {
                    dirty.push(Rect::new(x, y, w, h));
                }
            }
        }

        self.snapshot(frame_data, stride, row_bytes, packed_len);
        dirty
    }

    #[allow(clippy::too_many_arguments)]
    fn tile_differs(
        &self,
        frame_data: &[u8],
        stride: usize,
        x: u32,
        y: u32,
        w: u32,
        h: u32,
    ) -> bool {
        let bpp = self.bytes_per_pixel;
        let packed_row_bytes = self.width as usize * bpp;
        let row_len = w as usize * bpp;

        for row in 0..h as usize {
            let cur_off = (y as usize + row) * stride + x as usize * bpp;
            let prev_off = (y as usize + row) * packed_row_bytes + x as usize * bpp;

            // Slice equality uses an optimized memcmp, which is typically SIMD-accelerated.
            if frame_data[cur_off..cur_off + row_len]
                != self.prev_packed[prev_off..prev_off + row_len]
            {
                return true;
            }
        }
        false
    }

    fn snapshot(&mut self, frame_data: &[u8], stride: usize, row_bytes: usize, packed_len: usize) {
        debug_assert!(packed_len <= MAX_PREV_PACKED_BYTES);
        self.prev_packed.resize(packed_len, 0);

        for row in 0..self.height as usize {
            let src_off = row * stride;
            let dst_off = row * row_bytes;
            let len = row_bytes.min(frame_data.len().saturating_sub(src_off));
            self.prev_packed[dst_off..dst_off + len]
                .copy_from_slice(&frame_data[src_off..src_off + len]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_treats_overflow_row_bytes_as_full_frame_dirty() {
        let mut diff = TileDiff::new(1, 1, usize::MAX);
        let dirty = diff.diff(&[], 0);
        assert_eq!(dirty, vec![Rect::new(0, 0, 1, 1)]);
        assert!(diff.prev_packed.is_empty());
    }

    #[test]
    fn diff_rejects_huge_frames_without_allocating() {
        let width = u32::MAX;
        let height = 1;
        let mut diff = TileDiff::new(width, height, 4);
        let dirty = diff.diff(&[], 0);
        assert_eq!(dirty, vec![Rect::new(0, 0, width, height)]);
        assert!(diff.prev_packed.is_empty());
    }
}
