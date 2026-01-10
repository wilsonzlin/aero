use aero_gpu::{PresentError, Presenter, Rect, TextureWriter};
use pretty_assertions::assert_eq;

struct CpuTexture {
    width: u32,
    bytes_per_pixel: usize,
    data: Vec<u8>,
}

impl CpuTexture {
    fn new(width: u32, height: u32, bytes_per_pixel: usize) -> Self {
        Self {
            width,
            bytes_per_pixel,
            data: vec![0; width as usize * height as usize * bytes_per_pixel],
        }
    }
}

impl TextureWriter for CpuTexture {
    fn write_texture(&mut self, rect: Rect, bytes_per_row: usize, data: &[u8]) {
        let row_bytes = rect.w as usize * self.bytes_per_pixel;
        let dst_stride = self.width as usize * self.bytes_per_pixel;

        for row in 0..rect.h as usize {
            let src_off = row * bytes_per_row;
            let dst_off =
                (rect.y as usize + row) * dst_stride + rect.x as usize * self.bytes_per_pixel;
            self.data[dst_off..dst_off + row_bytes]
                .copy_from_slice(&data[src_off..src_off + row_bytes]);
        }
    }
}

fn fill_rect_rgba(
    frame: &mut [u8],
    stride: usize,
    rect: Rect,
    rgba: [u8; 4],
) {
    for y in rect.y..rect.y + rect.h {
        for x in rect.x..rect.x + rect.w {
            let off = y as usize * stride + x as usize * 4;
            frame[off..off + 4].copy_from_slice(&rgba);
        }
    }
}

#[test]
fn uploads_two_disjoint_rects_and_matches_expected_frame() -> Result<(), PresentError> {
    let width = 256u32;
    let height = 64u32;
    let bytes_per_pixel = 4usize;
    let stride = width as usize * bytes_per_pixel;

    let mut presenter = Presenter::new(width, height, bytes_per_pixel, CpuTexture::new(width, height, bytes_per_pixel));

    // Start with an all-black frame.
    let frame0 = vec![0u8; stride * height as usize];
    let full = presenter.present(&frame0, stride, None)?;
    assert_eq!(full.bytes_uploaded, frame0.len());
    assert_eq!(presenter.writer().data, frame0);

    // Update two disjoint rects in a new frame.
    let mut frame1 = frame0.clone();
    let r1 = Rect::new(0, 0, 64, 8);
    let r2 = Rect::new(128, 32, 64, 8);
    fill_rect_rgba(&mut frame1, stride, r1, [255, 0, 0, 255]);
    fill_rect_rgba(&mut frame1, stride, r2, [0, 255, 0, 255]);

    let partial = presenter.present(&frame1, stride, Some(&[r1, r2]))?;
    assert_eq!(partial.rects_requested, 2);
    assert_eq!(partial.rects_uploaded, 2);
    assert_eq!(partial.bytes_uploaded, 2048 * 2);

    // "Screenshot" (read-back of our CPU texture) should match the full frame contents.
    assert_eq!(presenter.writer().data, frame1);

    Ok(())
}
