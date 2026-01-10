pub fn movddup(src: u128) -> u128 {
    let low = src & 0xFFFF_FFFF_FFFF_FFFF;
    low | (low << 64)
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

