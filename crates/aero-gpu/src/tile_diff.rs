use crate::Rect;

const TILE_SIZE: u32 = 32;

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
        let row_bytes = self.width as usize * self.bytes_per_pixel;
        let packed_len = row_bytes * self.height as usize;

        if stride < row_bytes {
            // Malformed input; fall back to full frame.
            self.snapshot(frame_data, stride);
            return vec![Rect::new(0, 0, self.width, self.height)];
        }

        // If we don't have a previous frame (or size changed), treat as full dirty.
        if self.prev_packed.len() != packed_len {
            self.snapshot(frame_data, stride);
            return vec![Rect::new(0, 0, self.width, self.height)];
        }

        let tiles_x = (self.width + TILE_SIZE - 1) / TILE_SIZE;
        let tiles_y = (self.height + TILE_SIZE - 1) / TILE_SIZE;

        let mut dirty = Vec::new();
        for ty in 0..tiles_y {
            for tx in 0..tiles_x {
                let x = tx * TILE_SIZE;
                let y = ty * TILE_SIZE;
                let w = (self.width - x).min(TILE_SIZE);
                let h = (self.height - y).min(TILE_SIZE);

                if self.tile_differs(frame_data, stride, x, y, w, h, row_bytes) {
                    dirty.push(Rect::new(x, y, w, h));
                }
            }
        }

        self.snapshot(frame_data, stride);
        dirty
    }

    fn tile_differs(
        &self,
        frame_data: &[u8],
        stride: usize,
        x: u32,
        y: u32,
        w: u32,
        h: u32,
        packed_row_bytes: usize,
    ) -> bool {
        let bpp = self.bytes_per_pixel;
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

    fn snapshot(&mut self, frame_data: &[u8], stride: usize) {
        let row_bytes = self.width as usize * self.bytes_per_pixel;
        let packed_len = row_bytes * self.height as usize;
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

