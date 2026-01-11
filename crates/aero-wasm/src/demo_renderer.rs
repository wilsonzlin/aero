pub fn render_rgba8888(dst: &mut [u8], width: u32, height: u32, stride_bytes: u32, now_ms: f64) -> u32 {
    const BYTES_PER_PIXEL: usize = 4;

    if width == 0 || height == 0 {
        return 0;
    }

    let width_usize = width as usize;
    let height_usize = height as usize;

    let row_bytes = match width_usize.checked_mul(BYTES_PER_PIXEL) {
        Some(bytes) => bytes,
        None => return 0,
    };

    let mut stride = stride_bytes as usize;
    if stride < row_bytes {
        stride = row_bytes;
    }
    if stride == 0 {
        return 0;
    }

    // Clamp height to the provided destination slice so row addressing never
    // overflows.
    let max_height = dst.len() / stride;
    let draw_height = height_usize.min(max_height);
    if draw_height == 0 {
        return 0;
    }

    // Convert `now_ms` into integer offsets so the result is deterministic (and
    // stable across `wasm32` and host tests).
    let now_ms_u32 = if now_ms.is_finite() && now_ms > 0.0 {
        now_ms.min(u32::MAX as f64) as u32
    } else {
        0
    };
    let r_off = ((now_ms_u32 as u64).saturating_mul(60) / 1000) as u32;
    let g_off = ((now_ms_u32 as u64).saturating_mul(35) / 1000) as u32;
    let b_off = ((now_ms_u32 as u64).saturating_mul(20) / 1000) as u32;

    let base_ptr = dst.as_mut_ptr();

    for y in 0..draw_height {
        let y_u32 = y as u32;
        let row_base = y * stride;
        for x in 0..width_usize {
            let x_u32 = x as u32;

            let r = x_u32.wrapping_add(r_off) & 0xff;
            let g = y_u32.wrapping_add(g_off) & 0xff;
            let b = (x_u32 ^ y_u32).wrapping_add(b_off) & 0xff;

            // Write `[r, g, b, 255]` in little-endian form.
            let rgba = r | (g << 8) | (b << 16) | (0xff << 24);
            unsafe {
                core::ptr::write_unaligned(
                    base_ptr.add(row_base + x * BYTES_PER_PIXEL) as *mut u32,
                    rgba,
                );
            }
        }
    }

    (width as u64)
        .saturating_mul(draw_height as u64)
        .min(u32::MAX as u64) as u32
}

#[cfg(test)]
mod tests {
    use super::render_rgba8888;

    #[test]
    fn renders_known_pixels() {
        const W: u32 = 8;
        const H: u32 = 8;
        const STRIDE: u32 = W * 4;

        let mut buf = [0u8; (W * H * 4) as usize];
        let pixels = render_rgba8888(&mut buf, W, H, STRIDE, 1000.0);
        assert_eq!(pixels, W * H);

        // (0,0) at t=1.0s -> r=60, g=35, b=20, a=255
        assert_eq!(&buf[0..4], &[60, 35, 20, 255]);

        // (7,3): r=(7+60)=67, g=(3+35)=38, b=((7^3)+20)=(4+20)=24
        let idx = ((3 * STRIDE) + (7 * 4)) as usize;
        assert_eq!(&buf[idx..idx + 4], &[67, 38, 24, 255]);

        // (2,7): r=(2+60)=62, g=(7+35)=42, b=((2^7)+20)=(5+20)=25
        let idx = ((7 * STRIDE) + (2 * 4)) as usize;
        assert_eq!(&buf[idx..idx + 4], &[62, 42, 25, 255]);
    }
}
