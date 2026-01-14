//! Minimal BCn CPU decompression used for WebGL2 / capability fallback.
//!
//! The emulator workload frequently encounters BC-compressed textures (BC1/BC3/BC7).
//! When the GPU backend can't sample BC formats (e.g. WebGL2 fallback), we
//! deterministically decompress into RGBA8 on CPU and upload as `Rgba8Unorm*`.

/// For BC1/BC3 blocks we frequently decode full 4x4 tiles.
///
/// The "color indices" portion is encoded as 16 2-bit indices packed into a u32
/// in raster order. Each row is therefore an 8-bit value containing 4 indices.
///
/// In the SIMD fast paths we expand each row with a single byte-table lookup
/// (`PSHUFB` on x86/SSSE3, `i8x16.swizzle` on wasm SIMD128). This table stores
/// the swizzle mask for all possible row index patterns.
#[cfg(any(
    target_arch = "x86",
    target_arch = "x86_64",
    all(target_arch = "wasm32", target_feature = "simd128")
))]
const fn bc_row_shuffle_masks() -> [[u8; 16]; 256] {
    let mut table = [[0u8; 16]; 256];
    let mut row = 0usize;
    while row < 256 {
        let i0 = (row & 0b11) as u8;
        let i1 = ((row >> 2) & 0b11) as u8;
        let i2 = ((row >> 4) & 0b11) as u8;
        let i3 = ((row >> 6) & 0b11) as u8;

        let b0 = i0 * 4;
        let b1 = i1 * 4;
        let b2 = i2 * 4;
        let b3 = i3 * 4;

        table[row] = [
            b0,
            b0 + 1,
            b0 + 2,
            b0 + 3,
            b1,
            b1 + 1,
            b1 + 2,
            b1 + 3,
            b2,
            b2 + 1,
            b2 + 2,
            b2 + 3,
            b3,
            b3 + 1,
            b3 + 2,
            b3 + 3,
        ];
        row += 1;
    }
    table
}

#[cfg(any(
    target_arch = "x86",
    target_arch = "x86_64",
    all(target_arch = "wasm32", target_feature = "simd128")
))]
const BC_ROW_SHUFFLE_MASKS: [[u8; 16]; 256] = bc_row_shuffle_masks();

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

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
mod simd_x86 {
    #[cfg(target_arch = "x86")]
    use core::arch::x86::*;
    #[cfg(target_arch = "x86_64")]
    use core::arch::x86_64::*;

    use super::{
        decode_bc1_palette, decode_bc3_alpha_palette, decode_bc3_color_palette, BC_ROW_SHUFFLE_MASKS,
    };

    #[target_feature(enable = "sse2,ssse3")]
    pub unsafe fn decompress_bc1_block_full(block: &[u8; 8], dst: *mut u8, dst_stride: usize) {
        let color0 = u16::from_le_bytes([block[0], block[1]]);
        let color1 = u16::from_le_bytes([block[2], block[3]]);
        let indices = u32::from_le_bytes([block[4], block[5], block[6], block[7]]);

        let palette = decode_bc1_palette(color0, color1);
        let pal_bytes = [
            palette[0][0],
            palette[0][1],
            palette[0][2],
            palette[0][3],
            palette[1][0],
            palette[1][1],
            palette[1][2],
            palette[1][3],
            palette[2][0],
            palette[2][1],
            palette[2][2],
            palette[2][3],
            palette[3][0],
            palette[3][1],
            palette[3][2],
            palette[3][3],
        ];

        // Safe: unaligned loads/stores are permitted for movdqu.
        let pal = _mm_loadu_si128(pal_bytes.as_ptr() as *const __m128i);

        for row in 0..4usize {
            let row_idx = ((indices >> (row * 8)) & 0xFF) as usize;
            let mask = _mm_loadu_si128(BC_ROW_SHUFFLE_MASKS[row_idx].as_ptr() as *const __m128i);
            let out = _mm_shuffle_epi8(pal, mask);
            _mm_storeu_si128(dst.add(row * dst_stride) as *mut __m128i, out);
        }
    }

    #[target_feature(enable = "sse2,ssse3")]
    pub unsafe fn decompress_bc3_block_full(block: &[u8; 16], dst: *mut u8, dst_stride: usize) {
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

        let pal_bytes = [
            color_palette[0][0],
            color_palette[0][1],
            color_palette[0][2],
            0,
            color_palette[1][0],
            color_palette[1][1],
            color_palette[1][2],
            0,
            color_palette[2][0],
            color_palette[2][1],
            color_palette[2][2],
            0,
            color_palette[3][0],
            color_palette[3][1],
            color_palette[3][2],
            0,
        ];
        let pal = _mm_loadu_si128(pal_bytes.as_ptr() as *const __m128i);

        for row in 0..4usize {
            let row_idx = ((color_indices >> (row * 8)) & 0xFF) as usize;
            let mask = _mm_loadu_si128(BC_ROW_SHUFFLE_MASKS[row_idx].as_ptr() as *const __m128i);
            let mut out = _mm_shuffle_epi8(pal, mask);

            let row_alpha = (alpha_indices >> (row * 12)) & 0xFFF;
            let a0 = alpha_palette[(row_alpha & 0b111) as usize];
            let a1 = alpha_palette[((row_alpha >> 3) & 0b111) as usize];
            let a2 = alpha_palette[((row_alpha >> 6) & 0b111) as usize];
            let a3 = alpha_palette[((row_alpha >> 9) & 0b111) as usize];
            let alpha = _mm_set_epi32(
                (a3 as i32) << 24,
                (a2 as i32) << 24,
                (a1 as i32) << 24,
                (a0 as i32) << 24,
            );
            out = _mm_or_si128(out, alpha);

            _mm_storeu_si128(dst.add(row * dst_stride) as *mut __m128i, out);
        }
    }
}

#[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
mod simd_wasm {
    use core::arch::wasm32::*;

    use super::{
        decode_bc1_palette, decode_bc3_alpha_palette, decode_bc3_color_palette, BC_ROW_SHUFFLE_MASKS,
    };

    pub unsafe fn decompress_bc1_block_full(block: &[u8; 8], dst: *mut u8, dst_stride: usize) {
        let color0 = u16::from_le_bytes([block[0], block[1]]);
        let color1 = u16::from_le_bytes([block[2], block[3]]);
        let indices = u32::from_le_bytes([block[4], block[5], block[6], block[7]]);

        let palette = decode_bc1_palette(color0, color1);
        let pal_bytes = [
            palette[0][0],
            palette[0][1],
            palette[0][2],
            palette[0][3],
            palette[1][0],
            palette[1][1],
            palette[1][2],
            palette[1][3],
            palette[2][0],
            palette[2][1],
            palette[2][2],
            palette[2][3],
            palette[3][0],
            palette[3][1],
            palette[3][2],
            palette[3][3],
        ];
        let pal = v128_load(pal_bytes.as_ptr() as *const v128);

        for row in 0..4usize {
            let row_idx = ((indices >> (row * 8)) & 0xFF) as usize;
            let mask = v128_load(BC_ROW_SHUFFLE_MASKS[row_idx].as_ptr() as *const v128);
            let out = i8x16_swizzle(pal, mask);
            v128_store(dst.add(row * dst_stride) as *mut v128, out);
        }
    }

    pub unsafe fn decompress_bc3_block_full(block: &[u8; 16], dst: *mut u8, dst_stride: usize) {
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

        let pal_bytes = [
            color_palette[0][0],
            color_palette[0][1],
            color_palette[0][2],
            0,
            color_palette[1][0],
            color_palette[1][1],
            color_palette[1][2],
            0,
            color_palette[2][0],
            color_palette[2][1],
            color_palette[2][2],
            0,
            color_palette[3][0],
            color_palette[3][1],
            color_palette[3][2],
            0,
        ];
        let pal = v128_load(pal_bytes.as_ptr() as *const v128);

        for row in 0..4usize {
            let row_idx = ((color_indices >> (row * 8)) & 0xFF) as usize;
            let mask = v128_load(BC_ROW_SHUFFLE_MASKS[row_idx].as_ptr() as *const v128);
            let mut out = i8x16_swizzle(pal, mask);

            let row_alpha = (alpha_indices >> (row * 12)) & 0xFFF;
            let a0 = alpha_palette[(row_alpha & 0b111) as usize];
            let a1 = alpha_palette[((row_alpha >> 3) & 0b111) as usize];
            let a2 = alpha_palette[((row_alpha >> 6) & 0b111) as usize];
            let a3 = alpha_palette[((row_alpha >> 9) & 0b111) as usize];

            let alpha_lanes = [
                u32::from(a0) << 24,
                u32::from(a1) << 24,
                u32::from(a2) << 24,
                u32::from(a3) << 24,
            ];
            let alpha = v128_load(alpha_lanes.as_ptr() as *const v128);
            out = v128_or(out, alpha);

            v128_store(dst.add(row * dst_stride) as *mut v128, out);
        }
    }
}

/// Decompress BC1 texture data into an existing RGBA8 output buffer.
///
/// The caller must supply an output slice at least `width * height * 4` bytes long.
/// Any additional bytes in `out` are left untouched.
///
/// This is useful for benchmarking and other hot paths where the output allocation is
/// managed by the caller.
pub fn decompress_bc1_rgba8_into(width: u32, height: u32, bc1_data: &[u8], out: &mut [u8]) {
    let Some(out_len) = checked_decompressed_len_rgba8(width, height) else {
        return;
    };
    if out.len() < out_len {
        return;
    }
    let out = &mut out[..out_len];
    if out.is_empty() {
        return;
    }

    let blocks_w = width.div_ceil(4);
    if blocks_w == 0 {
        return;
    }

    // Only iterate the number of blocks actually present in the input buffer to avoid large
    // loops on malformed dimensions.
    let expected_bytes = checked_expected_bc_bytes(width, height, 8);
    let expected_blocks = expected_bytes.map(|b| b / 8).unwrap_or(0);
    let available_blocks = bc1_data.len() / 8;
    let blocks_to_process = expected_blocks.min(available_blocks);

    #[cfg(any(
        target_arch = "x86",
        target_arch = "x86_64",
        all(target_arch = "wasm32", target_feature = "simd128")
    ))]
    {
        let dst_stride = (width as usize) * 4;
        let simd_available = {
            #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
            {
                true
            }
            #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
            {
                std::arch::is_x86_feature_detected!("ssse3")
            }
        };

        for block_index in 0..blocks_to_process {
            let start = block_index * 8;
            let Some(block) = bc1_data
                .get(start..start + 8)
                .and_then(|slice| <&[u8; 8]>::try_from(slice).ok())
            else {
                break;
            };

            let bx = (block_index % blocks_w as usize) as u32;
            let by = (block_index / blocks_w as usize) as u32;
            let block_x = bx * 4;
            let block_y = by * 4;

            let full_block = u64::from(block_x) + 4 <= u64::from(width)
                && u64::from(block_y) + 4 <= u64::from(height);

            if simd_available && full_block {
                let dst_base = (u64::from(block_y) * u64::from(width) + u64::from(block_x)) * 4;
                let dst_base: usize = dst_base
                    .try_into()
                    .expect("pixel index should fit in usize for allocated output");

                unsafe {
                    #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
                    simd_wasm::decompress_bc1_block_full(
                        block,
                        out.as_mut_ptr().add(dst_base),
                        dst_stride,
                    );
                    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
                    simd_x86::decompress_bc1_block_full(
                        block,
                        out.as_mut_ptr().add(dst_base),
                        dst_stride,
                    );
                }
            } else {
                decompress_bc1_block(block, block_x, block_y, width, height, out);
            }
        }
    }

    #[cfg(not(any(
        target_arch = "x86",
        target_arch = "x86_64",
        all(target_arch = "wasm32", target_feature = "simd128")
    )))]
    {
        for block_index in 0..blocks_to_process {
            let start = block_index * 8;
            let Some(block) = bc1_data
                .get(start..start + 8)
                .and_then(|slice| <&[u8; 8]>::try_from(slice).ok())
            else {
                break;
            };

            let bx = (block_index % blocks_w as usize) as u32;
            let by = (block_index / blocks_w as usize) as u32;
            decompress_bc1_block(block, bx * 4, by * 4, width, height, out);
        }
    }
}

pub fn decompress_bc1_rgba8(width: u32, height: u32, bc1_data: &[u8]) -> Vec<u8> {
    let Some(out_len) = checked_decompressed_len_rgba8(width, height) else {
        return Vec::new();
    };
    // Output is zero-initialized so truncated input yields deterministic black texels.
    let mut out = vec![0u8; out_len];

    decompress_bc1_rgba8_into(width, height, bc1_data, &mut out);

    out
}

/// Decompress BC3 texture data into an existing RGBA8 output buffer.
///
/// The caller must supply an output slice at least `width * height * 4` bytes long.
/// Any additional bytes in `out` are left untouched.
pub fn decompress_bc3_rgba8_into(width: u32, height: u32, bc3_data: &[u8], out: &mut [u8]) {
    let Some(out_len) = checked_decompressed_len_rgba8(width, height) else {
        return;
    };
    if out.len() < out_len {
        return;
    }
    let out = &mut out[..out_len];
    if out.is_empty() {
        return;
    }

    let blocks_w = width.div_ceil(4);
    if blocks_w == 0 {
        return;
    }

    let expected_bytes = checked_expected_bc_bytes(width, height, 16);
    let expected_blocks = expected_bytes.map(|b| b / 16).unwrap_or(0);
    let available_blocks = bc3_data.len() / 16;
    let blocks_to_process = expected_blocks.min(available_blocks);

    #[cfg(any(
        target_arch = "x86",
        target_arch = "x86_64",
        all(target_arch = "wasm32", target_feature = "simd128")
    ))]
    {
        let dst_stride = (width as usize) * 4;
        let simd_available = {
            #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
            {
                true
            }
            #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
            {
                std::arch::is_x86_feature_detected!("ssse3")
            }
        };

        for block_index in 0..blocks_to_process {
            let start = block_index * 16;
            let Some(block) = bc3_data
                .get(start..start + 16)
                .and_then(|slice| <&[u8; 16]>::try_from(slice).ok())
            else {
                break;
            };

            let bx = (block_index % blocks_w as usize) as u32;
            let by = (block_index / blocks_w as usize) as u32;
            let block_x = bx * 4;
            let block_y = by * 4;

            let full_block = u64::from(block_x) + 4 <= u64::from(width)
                && u64::from(block_y) + 4 <= u64::from(height);

            if simd_available && full_block {
                let dst_base = (u64::from(block_y) * u64::from(width) + u64::from(block_x)) * 4;
                let dst_base: usize = dst_base
                    .try_into()
                    .expect("pixel index should fit in usize for allocated output");

                unsafe {
                    #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
                    simd_wasm::decompress_bc3_block_full(
                        block,
                        out.as_mut_ptr().add(dst_base),
                        dst_stride,
                    );
                    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
                    simd_x86::decompress_bc3_block_full(
                        block,
                        out.as_mut_ptr().add(dst_base),
                        dst_stride,
                    );
                }
            } else {
                decompress_bc3_block(block, block_x, block_y, width, height, out);
            }
        }
    }

    #[cfg(not(any(
        target_arch = "x86",
        target_arch = "x86_64",
        all(target_arch = "wasm32", target_feature = "simd128")
    )))]
    {
        for block_index in 0..blocks_to_process {
            let start = block_index * 16;
            let Some(block) = bc3_data
                .get(start..start + 16)
                .and_then(|slice| <&[u8; 16]>::try_from(slice).ok())
            else {
                break;
            };

            let bx = (block_index % blocks_w as usize) as u32;
            let by = (block_index / blocks_w as usize) as u32;
            decompress_bc3_block(block, bx * 4, by * 4, width, height, out);
        }
    }
}

pub fn decompress_bc3_rgba8(width: u32, height: u32, bc3_data: &[u8]) -> Vec<u8> {
    let Some(out_len) = checked_decompressed_len_rgba8(width, height) else {
        return Vec::new();
    };
    let mut out = vec![0u8; out_len];

    decompress_bc3_rgba8_into(width, height, bc3_data, &mut out);

    out
}

/// Decompress BC2 texture data into an existing RGBA8 output buffer.
///
/// The caller must supply an output slice at least `width * height * 4` bytes long.
/// Any additional bytes in `out` are left untouched.
pub fn decompress_bc2_rgba8_into(width: u32, height: u32, bc2_data: &[u8], out: &mut [u8]) {
    let Some(out_len) = checked_decompressed_len_rgba8(width, height) else {
        return;
    };
    if out.len() < out_len {
        return;
    }
    let out = &mut out[..out_len];

    let blocks_w = width.div_ceil(4);
    if blocks_w == 0 {
        return;
    }

    let expected_bytes = checked_expected_bc_bytes(width, height, 16);
    let expected_blocks = expected_bytes.map(|b| b / 16).unwrap_or(0);
    let available_blocks = bc2_data.len() / 16;
    let blocks_to_process = expected_blocks.min(available_blocks);

    for block_index in 0..blocks_to_process {
        let start = block_index * 16;
        let Some(block) = bc2_data
            .get(start..start + 16)
            .and_then(|slice| <&[u8; 16]>::try_from(slice).ok())
        else {
            break;
        };

        let bx = (block_index % blocks_w as usize) as u32;
        let by = (block_index / blocks_w as usize) as u32;
        decompress_bc2_block(block, bx * 4, by * 4, width, height, out);
    }
}

pub fn decompress_bc2_rgba8(width: u32, height: u32, bc2_data: &[u8]) -> Vec<u8> {
    let Some(out_len) = checked_decompressed_len_rgba8(width, height) else {
        return Vec::new();
    };
    let mut out = vec![0u8; out_len];

    decompress_bc2_rgba8_into(width, height, bc2_data, &mut out);

    out
}

/// Decompress BC7 texture data into an existing RGBA8 output buffer.
///
/// The caller must supply an output slice at least `width * height * 4` bytes long.
/// Any additional bytes in `out` are left untouched.
pub fn decompress_bc7_rgba8_into(width: u32, height: u32, bc7_data: &[u8], out: &mut [u8]) {
    let Some(out_len) = checked_decompressed_len_rgba8(width, height) else {
        return;
    };
    if out.len() < out_len {
        return;
    }
    let out = &mut out[..out_len];
    let mut decoded = [0u8; 4 * 4 * 4];

    let blocks_w = width.div_ceil(4);
    if blocks_w == 0 {
        return;
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
}

pub fn decompress_bc7_rgba8(width: u32, height: u32, bc7_data: &[u8]) -> Vec<u8> {
    let Some(out_len) = checked_decompressed_len_rgba8(width, height) else {
        return Vec::new();
    };
    let mut out = vec![0u8; out_len];

    decompress_bc7_rgba8_into(width, height, bc7_data, &mut out);

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(target_arch = "wasm32"))]
    use proptest::prelude::*;

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

    #[cfg(not(target_arch = "wasm32"))]
    fn decompress_bc1_rgba8_scalar(width: u32, height: u32, bc1_data: &[u8]) -> Vec<u8> {
        let Some(out_len) = checked_decompressed_len_rgba8(width, height) else {
            return Vec::new();
        };
        let mut out = vec![0u8; out_len];
        if out.is_empty() {
            return out;
        }

        let blocks_w = width.div_ceil(4);
        if blocks_w == 0 {
            return out;
        }

        let expected_bytes = checked_expected_bc_bytes(width, height, 8);
        let expected_blocks = expected_bytes.map(|b| b / 8).unwrap_or(0);
        let available_blocks = bc1_data.len() / 8;
        let blocks_to_process = expected_blocks.min(available_blocks);

        for block_index in 0..blocks_to_process {
            let start = block_index * 8;
            let Some(block) = bc1_data
                .get(start..start + 8)
                .and_then(|slice| <&[u8; 8]>::try_from(slice).ok())
            else {
                break;
            };

            let bx = (block_index % blocks_w as usize) as u32;
            let by = (block_index / blocks_w as usize) as u32;
            decompress_bc1_block(block, bx * 4, by * 4, width, height, &mut out);
        }

        out
    }

    #[cfg(all(
        not(target_arch = "wasm32"),
        any(target_arch = "x86", target_arch = "x86_64")
    ))]
    fn decompress_bc1_rgba8_simd(width: u32, height: u32, bc1_data: &[u8]) -> Option<Vec<u8>> {
        if !std::arch::is_x86_feature_detected!("ssse3") {
            return None;
        }

        let Some(out_len) = checked_decompressed_len_rgba8(width, height) else {
            return Some(Vec::new());
        };
        let mut out = vec![0u8; out_len];
        if out.is_empty() {
            return Some(out);
        }

        let blocks_w = width.div_ceil(4);
        if blocks_w == 0 {
            return Some(out);
        }

        let expected_bytes = checked_expected_bc_bytes(width, height, 8);
        let expected_blocks = expected_bytes.map(|b| b / 8).unwrap_or(0);
        let available_blocks = bc1_data.len() / 8;
        let blocks_to_process = expected_blocks.min(available_blocks);

        let dst_stride = (width as usize) * 4;

        for block_index in 0..blocks_to_process {
            let start = block_index * 8;
            let Some(block) = bc1_data
                .get(start..start + 8)
                .and_then(|slice| <&[u8; 8]>::try_from(slice).ok())
            else {
                break;
            };

            let bx = (block_index % blocks_w as usize) as u32;
            let by = (block_index / blocks_w as usize) as u32;
            let block_x = bx * 4;
            let block_y = by * 4;

            let full_block = u64::from(block_x) + 4 <= u64::from(width)
                && u64::from(block_y) + 4 <= u64::from(height);
            if full_block {
                let dst_base = (u64::from(block_y) * u64::from(width) + u64::from(block_x)) * 4;
                let dst_base: usize = dst_base
                    .try_into()
                    .expect("pixel index should fit in usize for allocated output");

                unsafe {
                    simd_x86::decompress_bc1_block_full(
                        block,
                        out.as_mut_ptr().add(dst_base),
                        dst_stride,
                    );
                }
            } else {
                decompress_bc1_block(block, block_x, block_y, width, height, &mut out);
            }
        }

        Some(out)
    }

    #[cfg(all(
        not(target_arch = "wasm32"),
        not(any(target_arch = "x86", target_arch = "x86_64"))
    ))]
    fn decompress_bc1_rgba8_simd(_width: u32, _height: u32, _bc1_data: &[u8]) -> Option<Vec<u8>> {
        None
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn decompress_bc3_rgba8_scalar(width: u32, height: u32, bc3_data: &[u8]) -> Vec<u8> {
        let Some(out_len) = checked_decompressed_len_rgba8(width, height) else {
            return Vec::new();
        };
        let mut out = vec![0u8; out_len];
        if out.is_empty() {
            return out;
        }

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
            let Some(block) = bc3_data
                .get(start..start + 16)
                .and_then(|slice| <&[u8; 16]>::try_from(slice).ok())
            else {
                break;
            };

            let bx = (block_index % blocks_w as usize) as u32;
            let by = (block_index / blocks_w as usize) as u32;
            decompress_bc3_block(block, bx * 4, by * 4, width, height, &mut out);
        }

        out
    }

    #[cfg(all(
        not(target_arch = "wasm32"),
        any(target_arch = "x86", target_arch = "x86_64")
    ))]
    fn decompress_bc3_rgba8_simd(width: u32, height: u32, bc3_data: &[u8]) -> Option<Vec<u8>> {
        if !std::arch::is_x86_feature_detected!("ssse3") {
            return None;
        }

        let Some(out_len) = checked_decompressed_len_rgba8(width, height) else {
            return Some(Vec::new());
        };
        let mut out = vec![0u8; out_len];
        if out.is_empty() {
            return Some(out);
        }

        let blocks_w = width.div_ceil(4);
        if blocks_w == 0 {
            return Some(out);
        }

        let expected_bytes = checked_expected_bc_bytes(width, height, 16);
        let expected_blocks = expected_bytes.map(|b| b / 16).unwrap_or(0);
        let available_blocks = bc3_data.len() / 16;
        let blocks_to_process = expected_blocks.min(available_blocks);

        let dst_stride = (width as usize) * 4;

        for block_index in 0..blocks_to_process {
            let start = block_index * 16;
            let Some(block) = bc3_data
                .get(start..start + 16)
                .and_then(|slice| <&[u8; 16]>::try_from(slice).ok())
            else {
                break;
            };

            let bx = (block_index % blocks_w as usize) as u32;
            let by = (block_index / blocks_w as usize) as u32;
            let block_x = bx * 4;
            let block_y = by * 4;

            let full_block = u64::from(block_x) + 4 <= u64::from(width)
                && u64::from(block_y) + 4 <= u64::from(height);
            if full_block {
                let dst_base = (u64::from(block_y) * u64::from(width) + u64::from(block_x)) * 4;
                let dst_base: usize = dst_base
                    .try_into()
                    .expect("pixel index should fit in usize for allocated output");

                unsafe {
                    simd_x86::decompress_bc3_block_full(
                        block,
                        out.as_mut_ptr().add(dst_base),
                        dst_stride,
                    );
                }
            } else {
                decompress_bc3_block(block, block_x, block_y, width, height, &mut out);
            }
        }

        Some(out)
    }

    #[cfg(all(
        not(target_arch = "wasm32"),
        not(any(target_arch = "x86", target_arch = "x86_64"))
    ))]
    fn decompress_bc3_rgba8_simd(_width: u32, _height: u32, _bc3_data: &[u8]) -> Option<Vec<u8>> {
        None
    }

    #[cfg(not(target_arch = "wasm32"))]
    proptest! {
        #[test]
        fn bc1_simd_matches_scalar(
            case in (0u32..=16, 0u32..=16).prop_flat_map(|(w, h)| {
                let expected = checked_expected_bc_bytes(w, h, 8).unwrap_or(0);
                let max_len = expected.saturating_add(16);
                (Just((w, h)), proptest::collection::vec(any::<u8>(), 0..=max_len))
            })
        ) {
            let ((w, h), data) = case;
            let scalar = decompress_bc1_rgba8_scalar(w, h, &data);

            if let Some(simd) = decompress_bc1_rgba8_simd(w, h, &data) {
                prop_assert_eq!(simd, scalar);
            } else {
                prop_assert_eq!(decompress_bc1_rgba8(w, h, &data), scalar);
            }
        }

        #[test]
        fn bc3_simd_matches_scalar(
            case in (0u32..=16, 0u32..=16).prop_flat_map(|(w, h)| {
                let expected = checked_expected_bc_bytes(w, h, 16).unwrap_or(0);
                let max_len = expected.saturating_add(16);
                (Just((w, h)), proptest::collection::vec(any::<u8>(), 0..=max_len))
            })
        ) {
            let ((w, h), data) = case;
            let scalar = decompress_bc3_rgba8_scalar(w, h, &data);

            if let Some(simd) = decompress_bc3_rgba8_simd(w, h, &data) {
                prop_assert_eq!(simd, scalar);
            } else {
                prop_assert_eq!(decompress_bc3_rgba8(w, h, &data), scalar);
            }
        }
    }
}
