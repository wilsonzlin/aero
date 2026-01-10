pub fn movddup(src: u128) -> u128 {
    let low = src & 0xFFFF_FFFF_FFFF_FFFF;
    low | (low << 64)
}

fn as_f32x4(x: u128) -> [f32; 4] {
    let bytes = x.to_le_bytes();
    let mut out = [0f32; 4];
    for (i, chunk) in bytes.chunks_exact(4).enumerate() {
        out[i] = f32::from_bits(u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }
    out
}

fn from_f32x4(v: [f32; 4]) -> u128 {
    let mut out = [0u8; 16];
    for (i, lane) in v.into_iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&lane.to_bits().to_le_bytes());
    }
    u128::from_le_bytes(out)
}

fn as_f64x2(x: u128) -> [f64; 2] {
    let bytes = x.to_le_bytes();
    let mut out = [0f64; 2];
    for (i, chunk) in bytes.chunks_exact(8).enumerate() {
        out[i] = f64::from_bits(u64::from_le_bytes([
            chunk[0], chunk[1], chunk[2], chunk[3], chunk[4], chunk[5], chunk[6], chunk[7],
        ]));
    }
    out
}

fn from_f64x2(v: [f64; 2]) -> u128 {
    let mut out = [0u8; 16];
    for (i, lane) in v.into_iter().enumerate() {
        out[i * 8..i * 8 + 8].copy_from_slice(&lane.to_bits().to_le_bytes());
    }
    u128::from_le_bytes(out)
}

pub fn movsldup(src: u128) -> u128 {
    let src = src.to_le_bytes();
    let mut out = [0u8; 16];
    out[0..4].copy_from_slice(&src[0..4]);
    out[4..8].copy_from_slice(&src[0..4]);
    out[8..12].copy_from_slice(&src[8..12]);
    out[12..16].copy_from_slice(&src[8..12]);
    u128::from_le_bytes(out)
}

pub fn movshdup(src: u128) -> u128 {
    let src = src.to_le_bytes();
    let mut out = [0u8; 16];
    out[0..4].copy_from_slice(&src[4..8]);
    out[4..8].copy_from_slice(&src[4..8]);
    out[8..12].copy_from_slice(&src[12..16]);
    out[12..16].copy_from_slice(&src[12..16]);
    u128::from_le_bytes(out)
}

pub fn haddps(dst: u128, src: u128) -> u128 {
    let a = as_f32x4(dst);
    let b = as_f32x4(src);
    from_f32x4([a[0] + a[1], a[2] + a[3], b[0] + b[1], b[2] + b[3]])
}

pub fn hsubps(dst: u128, src: u128) -> u128 {
    let a = as_f32x4(dst);
    let b = as_f32x4(src);
    from_f32x4([a[0] - a[1], a[2] - a[3], b[0] - b[1], b[2] - b[3]])
}

pub fn haddpd(dst: u128, src: u128) -> u128 {
    let a = as_f64x2(dst);
    let b = as_f64x2(src);
    from_f64x2([a[0] + a[1], b[0] + b[1]])
}

pub fn hsubpd(dst: u128, src: u128) -> u128 {
    let a = as_f64x2(dst);
    let b = as_f64x2(src);
    from_f64x2([a[0] - a[1], b[0] - b[1]])
}
