//! Helpers for expanding/packing legacy 16-bit packed pixel formats.
//!
//! AeroGPU exposes a couple of 16bpp formats that are common in scanout/legacy surfaces:
//! - `B5G6R5Unorm` (RGB565 in little-endian)
//! - `B5G5R5A1Unorm` (ARGB1555-like layout in little-endian, but with B in LSB)
//!
//! wgpu/WebGPU does not expose these formats, so higher layers represent them as RGBA8 textures and
//! perform CPU-side conversion on upload/readback.

/// Expand packed `B5G6R5Unorm` bytes into RGBA8.
///
/// Input layout: little-endian u16 where bits are:
/// - 0..4   = B
/// - 5..10  = G
/// - 11..15 = R
///
/// Output layout: `[R, G, B, A]` with `A=0xFF`.
pub fn expand_b5g6r5_unorm_to_rgba8(src: &[u8], dst: &mut [u8]) {
    debug_assert_eq!(src.len() % 2, 0);
    debug_assert_eq!(dst.len(), (src.len() / 2) * 4);
    for (src_px, dst_px) in src.chunks_exact(2).zip(dst.chunks_exact_mut(4)) {
        let v = u16::from_le_bytes([src_px[0], src_px[1]]);
        let b5 = (v & 0x1F) as u8;
        let g6 = ((v >> 5) & 0x3F) as u8;
        let r5 = ((v >> 11) & 0x1F) as u8;
        // Replicate bits to fill the 8-bit range.
        let r8 = (r5 << 3) | (r5 >> 2);
        let g8 = (g6 << 2) | (g6 >> 4);
        let b8 = (b5 << 3) | (b5 >> 2);
        dst_px[0] = r8;
        dst_px[1] = g8;
        dst_px[2] = b8;
        dst_px[3] = 0xFF;
    }
}

/// Expand packed `B5G5R5A1Unorm` bytes into RGBA8.
///
/// Input layout: little-endian u16 where bits are:
/// - 0..4   = B
/// - 5..9   = G
/// - 10..14 = R
/// - 15     = A (1-bit)
///
/// Output layout: `[R, G, B, A]` where `A` is `0xFF` if the bit is set, otherwise `0x00`.
pub fn expand_b5g5r5a1_unorm_to_rgba8(src: &[u8], dst: &mut [u8]) {
    debug_assert_eq!(src.len() % 2, 0);
    debug_assert_eq!(dst.len(), (src.len() / 2) * 4);
    for (src_px, dst_px) in src.chunks_exact(2).zip(dst.chunks_exact_mut(4)) {
        let v = u16::from_le_bytes([src_px[0], src_px[1]]);
        let b5 = (v & 0x1F) as u8;
        let g5 = ((v >> 5) & 0x1F) as u8;
        let r5 = ((v >> 10) & 0x1F) as u8;
        let a1 = (v >> 15) as u8;
        let r8 = (r5 << 3) | (r5 >> 2);
        let g8 = (g5 << 3) | (g5 >> 2);
        let b8 = (b5 << 3) | (b5 >> 2);
        dst_px[0] = r8;
        dst_px[1] = g8;
        dst_px[2] = b8;
        dst_px[3] = if a1 != 0 { 0xFF } else { 0x00 };
    }
}

/// Pack RGBA8 bytes into `B5G6R5Unorm` (little-endian u16).
pub fn pack_rgba8_to_b5g6r5_unorm(src: &[u8], dst: &mut [u8]) {
    debug_assert_eq!(src.len() % 4, 0);
    debug_assert_eq!(dst.len(), (src.len() / 4) * 2);
    for (src_px, dst_px) in src.chunks_exact(4).zip(dst.chunks_exact_mut(2)) {
        let r8 = src_px[0];
        let g8 = src_px[1];
        let b8 = src_px[2];
        let r5 = (r8 >> 3) as u16;
        let g6 = (g8 >> 2) as u16;
        let b5 = (b8 >> 3) as u16;
        let v: u16 = b5 | (g6 << 5) | (r5 << 11);
        let out = v.to_le_bytes();
        dst_px[0] = out[0];
        dst_px[1] = out[1];
    }
}

/// Pack RGBA8 bytes into `B5G5R5A1Unorm` (little-endian u16).
pub fn pack_rgba8_to_b5g5r5a1_unorm(src: &[u8], dst: &mut [u8]) {
    debug_assert_eq!(src.len() % 4, 0);
    debug_assert_eq!(dst.len(), (src.len() / 4) * 2);
    for (src_px, dst_px) in src.chunks_exact(4).zip(dst.chunks_exact_mut(2)) {
        let r8 = src_px[0];
        let g8 = src_px[1];
        let b8 = src_px[2];
        let a8 = src_px[3];
        let r5 = (r8 >> 3) as u16;
        let g5 = (g8 >> 3) as u16;
        let b5 = (b8 >> 3) as u16;
        let a1 = if a8 >= 0x80 { 1u16 } else { 0u16 };
        let v: u16 = b5 | (g5 << 5) | (r5 << 10) | (a1 << 15);
        let out = v.to_le_bytes();
        dst_px[0] = out[0];
        dst_px[1] = out[1];
    }
}
