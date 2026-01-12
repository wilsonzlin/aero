//! Minimal BCn CPU decompression used for WebGL2 / capability fallback.
//!
//! The emulator workload frequently encounters BC-compressed textures (BC1/BC3/BC7).
//! When the GPU backend can't sample BC formats (e.g. WebGL2 fallback), we
//! deterministically decompress into RGBA8 on CPU and upload as `Rgba8Unorm*`.

fn rgb565_to_rgb888(c: u16) -> [u8; 3] {
    let r5 = ((c >> 11) & 0x1f) as u8;
    let g6 = ((c >> 5) & 0x3f) as u8;
    let b5 = (c & 0x1f) as u8;

    // Replicate top bits into low bits to fill 8-bit channels.
    let r = (r5 << 3) | (r5 >> 2);
    let g = (g6 << 2) | (g6 >> 4);
    let b = (b5 << 3) | (b5 >> 2);
    [r, g, b]
}

fn lerp_u8(a: u8, b: u8, num: u32, den: u32) -> u8 {
    debug_assert!(num <= den);
    (((a as u32) * (den - num) + (b as u32) * num) / den) as u8
}

fn decode_bc1_palette(color0: u16, color1: u16) -> [[u8; 4]; 4] {
    let c0 = rgb565_to_rgb888(color0);
    let c1 = rgb565_to_rgb888(color1);

    let mut palette = [[0u8; 4]; 4];
    palette[0] = [c0[0], c0[1], c0[2], 255];
    palette[1] = [c1[0], c1[1], c1[2], 255];

    if color0 > color1 {
        // 4-color mode
        palette[2] = [
            lerp_u8(c0[0], c1[0], 1, 3),
            lerp_u8(c0[1], c1[1], 1, 3),
            lerp_u8(c0[2], c1[2], 1, 3),
            255,
        ];
        palette[3] = [
            lerp_u8(c0[0], c1[0], 2, 3),
            lerp_u8(c0[1], c1[1], 2, 3),
            lerp_u8(c0[2], c1[2], 2, 3),
            255,
        ];
    } else {
        // 3-color mode + transparent
        palette[2] = [
            lerp_u8(c0[0], c1[0], 1, 2),
            lerp_u8(c0[1], c1[1], 1, 2),
            lerp_u8(c0[2], c1[2], 1, 2),
            255,
        ];
        palette[3] = [0, 0, 0, 0];
    }

    palette
}

fn decode_bc3_color_palette(color0: u16, color1: u16) -> [[u8; 3]; 4] {
    let c0 = rgb565_to_rgb888(color0);
    let c1 = rgb565_to_rgb888(color1);

    // BC3 color block is effectively BC1 "opaque" mode (no 1-bit alpha).
    let c2 = [
        lerp_u8(c0[0], c1[0], 1, 3),
        lerp_u8(c0[1], c1[1], 1, 3),
        lerp_u8(c0[2], c1[2], 1, 3),
    ];
    let c3 = [
        lerp_u8(c0[0], c1[0], 2, 3),
        lerp_u8(c0[1], c1[1], 2, 3),
        lerp_u8(c0[2], c1[2], 2, 3),
    ];

    [c0, c1, c2, c3]
}

fn decompress_bc2_block(
    block: &[u8; 16],
    block_x: u32,
    block_y: u32,
    width: u32,
    height: u32,
    out: &mut [u8],
) {
    let alpha_bits = u64::from_le_bytes(block[0..8].try_into().unwrap());

    let color0 = u16::from_le_bytes([block[8], block[9]]);
    let color1 = u16::from_le_bytes([block[10], block[11]]);
    let indices = u32::from_le_bytes([block[12], block[13], block[14], block[15]]);
    let palette = decode_bc3_color_palette(color0, color1);

    for i in 0..16u32 {
        let a4 = ((alpha_bits >> (4 * i)) & 0xF) as u8;
        let alpha = a4 * 17;

        let c_idx = ((indices >> (2 * i)) & 0b11) as usize;
        let rgb = palette[c_idx];

        let px = block_x + (i % 4);
        let py = block_y + (i / 4);
        if px < width && py < height {
            write_pixel_rgb_a(out, width, px, py, rgb, alpha);
        }
    }
}

fn decode_bc3_alpha_palette(alpha0: u8, alpha1: u8) -> [u8; 8] {
    let mut a = [0u8; 8];
    a[0] = alpha0;
    a[1] = alpha1;
    if alpha0 > alpha1 {
        // 8-alpha mode.
        a[2] = lerp_u8(alpha0, alpha1, 1, 7);
        a[3] = lerp_u8(alpha0, alpha1, 2, 7);
        a[4] = lerp_u8(alpha0, alpha1, 3, 7);
        a[5] = lerp_u8(alpha0, alpha1, 4, 7);
        a[6] = lerp_u8(alpha0, alpha1, 5, 7);
        a[7] = lerp_u8(alpha0, alpha1, 6, 7);
    } else {
        // 6-alpha mode with explicit 0 and 255.
        a[2] = lerp_u8(alpha0, alpha1, 1, 5);
        a[3] = lerp_u8(alpha0, alpha1, 2, 5);
        a[4] = lerp_u8(alpha0, alpha1, 3, 5);
        a[5] = lerp_u8(alpha0, alpha1, 4, 5);
        a[6] = 0;
        a[7] = 255;
    }
    a
}

fn write_pixel(out: &mut [u8], width: u32, x: u32, y: u32, rgba: [u8; 4]) {
    // Use u64 arithmetic to avoid overflow panics when called with extreme dimensions.
    let idx = (u64::from(y) * u64::from(width) + u64::from(x)) * 4;
    let idx: usize = idx
        .try_into()
        .expect("pixel index should fit in usize for allocated output");
    out[idx..idx + 4].copy_from_slice(&rgba);
}

fn write_pixel_rgb_a(out: &mut [u8], width: u32, x: u32, y: u32, rgb: [u8; 3], a: u8) {
    let idx = (u64::from(y) * u64::from(width) + u64::from(x)) * 4;
    let idx: usize = idx
        .try_into()
        .expect("pixel index should fit in usize for allocated output");
    out[idx] = rgb[0];
    out[idx + 1] = rgb[1];
    out[idx + 2] = rgb[2];
    out[idx + 3] = a;
}

fn checked_decompressed_len_rgba8(width: u32, height: u32) -> Option<usize> {
    let pixels = u64::from(width).checked_mul(u64::from(height))?;
    let bytes = pixels.checked_mul(4)?;
    usize::try_from(bytes).ok()
}

fn checked_expected_bc_bytes(width: u32, height: u32, bytes_per_block: u64) -> Option<usize> {
    let blocks_w = u64::from(width.div_ceil(4));
    let blocks_h = u64::from(height.div_ceil(4));
    let blocks = blocks_w.checked_mul(blocks_h)?;
    let bytes = blocks.checked_mul(bytes_per_block)?;
    usize::try_from(bytes).ok()
}

fn decompress_bc1_block(
    block: &[u8; 8],
    block_x: u32,
    block_y: u32,
    width: u32,
    height: u32,
    out: &mut [u8],
) {
    let color0 = u16::from_le_bytes([block[0], block[1]]);
    let color1 = u16::from_le_bytes([block[2], block[3]]);
    let indices = u32::from_le_bytes([block[4], block[5], block[6], block[7]]);

    let palette = decode_bc1_palette(color0, color1);

    for i in 0..16u32 {
        let idx = ((indices >> (2 * i)) & 0b11) as usize;
        let px = block_x + (i % 4);
        let py = block_y + (i / 4);
        if px < width && py < height {
            write_pixel(out, width, px, py, palette[idx]);
        }
    }
}

fn decompress_bc3_block(
    block: &[u8; 16],
    block_x: u32,
    block_y: u32,
    width: u32,
    height: u32,
    out: &mut [u8],
) {
    let alpha0 = block[0];
    let alpha1 = block[1];
    let alpha_palette = decode_bc3_alpha_palette(alpha0, alpha1);

    // 48 bits, little-endian.
    let mut alpha_indices: u64 = 0;
    for (i, b) in block[2..8].iter().enumerate() {
        alpha_indices |= (*b as u64) << (8 * i);
    }

    let color0 = u16::from_le_bytes([block[8], block[9]]);
    let color1 = u16::from_le_bytes([block[10], block[11]]);
    let color_indices = u32::from_le_bytes([block[12], block[13], block[14], block[15]]);
    let color_palette = decode_bc3_color_palette(color0, color1);

    for i in 0..16u32 {
        let a_idx = ((alpha_indices >> (3 * i)) & 0b111) as usize;
        let alpha = alpha_palette[a_idx];

        let c_idx = ((color_indices >> (2 * i)) & 0b11) as usize;
        let rgb = color_palette[c_idx];

        let px = block_x + (i % 4);
        let py = block_y + (i / 4);
        if px < width && py < height {
            write_pixel_rgb_a(out, width, px, py, rgb, alpha);
        }
    }
}

pub fn decompress_bc1_rgba8(width: u32, height: u32, bc1_data: &[u8]) -> Vec<u8> {
    let Some(out_len) = checked_decompressed_len_rgba8(width, height) else {
        return Vec::new();
    };
    // Output is zero-initialized so truncated input yields deterministic black texels.
    let mut out = vec![0u8; out_len];

    let blocks_w = width.div_ceil(4);
    if blocks_w == 0 {
        return out;
    }

    // Only iterate the number of blocks actually present in the input buffer to avoid large
    // loops on malformed dimensions.
    let expected_bytes = checked_expected_bc_bytes(width, height, 8);
    let expected_blocks = expected_bytes.map(|b| b / 8).unwrap_or(0);
    let available_blocks = bc1_data.len() / 8;
    let blocks_to_process = expected_blocks.min(available_blocks);

    for block_index in 0..blocks_to_process {
        let start = block_index * 8;
        let Ok(block) = bc1_data[start..start + 8].try_into() else {
            break;
        };

        let bx = (block_index % blocks_w as usize) as u32;
        let by = (block_index / blocks_w as usize) as u32;
        decompress_bc1_block(&block, bx * 4, by * 4, width, height, &mut out);
    }

    out
}

pub fn decompress_bc3_rgba8(width: u32, height: u32, bc3_data: &[u8]) -> Vec<u8> {
    let Some(out_len) = checked_decompressed_len_rgba8(width, height) else {
        return Vec::new();
    };
    let mut out = vec![0u8; out_len];

    let blocks_w = width.div_ceil(4);
    if blocks_w == 0 {
        return out;
    }

    let expected_bytes = checked_expected_bc_bytes(width, height, 16);
    let expected_blocks = expected_bytes.map(|b| b / 16).unwrap_or(0);
    let available_blocks = bc3_data.len() / 16;
    let blocks_to_process = expected_blocks.min(available_blocks);

    for block_index in 0..blocks_to_process {
        let start = block_index * 16;
        let Ok(block) = bc3_data[start..start + 16].try_into() else {
            break;
        };

        let bx = (block_index % blocks_w as usize) as u32;
        let by = (block_index / blocks_w as usize) as u32;
        decompress_bc3_block(&block, bx * 4, by * 4, width, height, &mut out);
    }

    out
}

pub fn decompress_bc2_rgba8(width: u32, height: u32, bc2_data: &[u8]) -> Vec<u8> {
    let Some(out_len) = checked_decompressed_len_rgba8(width, height) else {
        return Vec::new();
    };
    let mut out = vec![0u8; out_len];

    let blocks_w = width.div_ceil(4);
    if blocks_w == 0 {
        return out;
    }

    let expected_bytes = checked_expected_bc_bytes(width, height, 16);
    let expected_blocks = expected_bytes.map(|b| b / 16).unwrap_or(0);
    let available_blocks = bc2_data.len() / 16;
    let blocks_to_process = expected_blocks.min(available_blocks);

    for block_index in 0..blocks_to_process {
        let start = block_index * 16;
        let Ok(block) = bc2_data[start..start + 16].try_into() else {
            break;
        };

        let bx = (block_index % blocks_w as usize) as u32;
        let by = (block_index / blocks_w as usize) as u32;
        decompress_bc2_block(&block, bx * 4, by * 4, width, height, &mut out);
    }

    out
}

pub fn decompress_bc7_rgba8(width: u32, height: u32, bc7_data: &[u8]) -> Vec<u8> {
    let Some(out_len) = checked_decompressed_len_rgba8(width, height) else {
        return Vec::new();
    };
    let mut out = vec![0u8; out_len];
    let mut decoded = [0u8; 4 * 4 * 4];

    let blocks_w = width.div_ceil(4);
    if blocks_w == 0 {
        return out;
    }

    let expected_bytes = checked_expected_bc_bytes(width, height, 16);
    let expected_blocks = expected_bytes.map(|b| b / 16).unwrap_or(0);
    let available_blocks = bc7_data.len() / 16;
    let blocks_to_process = expected_blocks.min(available_blocks);

    for block_index in 0..blocks_to_process {
        let start = block_index * 16;
        let Some(block) = bc7_data.get(start..start + 16) else {
            break;
        };

        bcdec_rs::bc7(block, &mut decoded, 4 * 4);

        let bx = (block_index % blocks_w as usize) as u32;
        let by = (block_index / blocks_w as usize) as u32;

        for py in 0..4u32 {
            for px in 0..4u32 {
                let x = bx * 4 + px;
                let y = by * 4 + py;
                if x >= width || y >= height {
                    continue;
                }

                let src = ((py * 16 + px * 4) as usize)..((py * 16 + px * 4 + 4) as usize);
                let dst = (u64::from(y) * u64::from(width) + u64::from(x)) * 4;
                let dst: usize = dst
                    .try_into()
                    .expect("pixel index should fit in usize for allocated output");
                out[dst..dst + 4].copy_from_slice(&decoded[src]);
            }
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bc1_known_vector_four_color_mode() {
        // color0=0xffff (white), color1=0x0000 (black), indices:
        // row0 -> 0 (white)
        // row1 -> 1 (black)
        // row2 -> 2 (2/3 white)
        // row3 -> 3 (1/3 white)
        let bc1 = [
            0xff, 0xff, // color0
            0x00, 0x00, // color1
            0x00, 0x55, 0xaa, 0xff, // indices (little-endian u32)
        ];

        let rgba = decompress_bc1_rgba8(4, 4, &bc1);

        let mut expected = Vec::new();
        // row0: white
        expected.extend_from_slice(&[255, 255, 255, 255].repeat(4));
        // row1: black
        expected.extend_from_slice(&[0, 0, 0, 255].repeat(4));
        // row2: 170 gray
        expected.extend_from_slice(&[170, 170, 170, 255].repeat(4));
        // row3: 85 gray
        expected.extend_from_slice(&[85, 85, 85, 255].repeat(4));

        assert_eq!(rgba, expected);
    }

    #[test]
    fn bc1_known_vector_three_color_mode_with_transparent() {
        // Trigger 3-color mode by making color0 <= color1.
        // color0=0x0000 (black), color1=0xffff (white).
        // indices: first texel uses index 3 (transparent), rest index 0 (black).
        let idx_bytes = 3u32.to_le_bytes();
        let bc1 = [
            0x00,
            0x00, // color0
            0xff,
            0xff, // color1
            idx_bytes[0],
            idx_bytes[1],
            idx_bytes[2],
            idx_bytes[3],
        ];

        let rgba = decompress_bc1_rgba8(4, 4, &bc1);
        assert_eq!(&rgba[0..4], &[0, 0, 0, 0]); // transparent
        assert_eq!(&rgba[4..8], &[0, 0, 0, 255]); // black
    }

    #[test]
    fn bc3_known_vector_alpha_interpolation() {
        // Alpha palette: alpha0=255, alpha1=0 with row-wise indices:
        // row0 -> idx0 (255)
        // row1 -> idx1 (0)
        // row2 -> idx2 (~218)
        // row3 -> idx7 (~36)
        let bc3 = [
            0xff, 0x00, // alpha0, alpha1
            0x00, 0x90, 0x24, 0x92, 0xf4, 0xff, // alpha indices (48-bit LE)
            0xff, 0xff, // color0 (white)
            0x00, 0x00, // color1 (black)
            0x00, 0x00, 0x00, 0x00, // color indices (all 0 -> white)
        ];

        let rgba = decompress_bc3_rgba8(4, 4, &bc3);

        // Row 0 alpha 255.
        assert_eq!(&rgba[0..4], &[255, 255, 255, 255]);
        // Row 1 alpha 0.
        let row_stride = 4 * 4;
        let row1 = row_stride;
        assert_eq!(&rgba[row1..row1 + 4], &[255, 255, 255, 0]);
        // Row 2 alpha 218 (floor(6*255/7)).
        let row2 = 2 * row_stride;
        assert_eq!(&rgba[row2..row2 + 4], &[255, 255, 255, 218]);
        // Row 3 alpha 36 (floor(1*255/7)).
        let row3 = 3 * row_stride;
        assert_eq!(&rgba[row3..row3 + 4], &[255, 255, 255, 36]);
    }

    #[test]
    fn bc2_known_vector_explicit_alpha() {
        // Alpha uses explicit 4-bit values. Construct row-wise pattern:
        // row0 -> 0xF (255)
        // row1 -> 0x0 (0)
        // row2 -> 0x8 (136)
        // row3 -> 0x1 (17)
        let bc2 = [
            0xff, 0xff, 0x00, 0x00, 0x88, 0x88, 0x11, 0x11, // alpha bits (LE u64)
            0xff, 0xff, // color0 (white)
            0xff, 0xff, // color1 (white)
            0x00, 0x00, 0x00, 0x00, // indices (all 0 -> white)
        ];

        let rgba = decompress_bc2_rgba8(4, 4, &bc2);

        // Row 0 alpha 255.
        assert_eq!(&rgba[0..4], &[255, 255, 255, 255]);
        // Row 1 alpha 0.
        let row_stride = 4 * 4;
        let row1 = row_stride;
        assert_eq!(&rgba[row1..row1 + 4], &[255, 255, 255, 0]);
        // Row 2 alpha 136.
        let row2 = 2 * row_stride;
        assert_eq!(&rgba[row2..row2 + 4], &[255, 255, 255, 136]);
        // Row 3 alpha 17.
        let row3 = 3 * row_stride;
        assert_eq!(&rgba[row3..row3 + 4], &[255, 255, 255, 17]);
    }

    #[test]
    fn bc1_short_input_is_zero_filled() {
        // 4x4 BC1 expects exactly 8 bytes but provide fewer. We should not panic.
        let rgba = decompress_bc1_rgba8(4, 4, &[0u8; 4]);
        assert_eq!(rgba.len(), 4 * 4 * 4);
        assert!(rgba.iter().all(|&b| b == 0));
    }

    #[test]
    fn bc7_huge_dimensions_do_not_overflow_or_hang() {
        // The decoder should return quickly without attempting to allocate or iterate a massive
        // output buffer on obviously-invalid dimensions.
        let rgba = decompress_bc7_rgba8(u32::MAX, u32::MAX, &[]);
        assert!(rgba.is_empty());
    }
}
