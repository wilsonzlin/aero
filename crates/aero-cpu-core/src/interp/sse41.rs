fn from_i16x8(v: [i16; 8]) -> u128 {
    let mut out = [0u8; 16];
    for (i, lane) in v.into_iter().enumerate() {
        out[i * 2..i * 2 + 2].copy_from_slice(&lane.to_le_bytes());
    }
    u128::from_le_bytes(out)
}

fn as_i32x4(x: u128) -> [i32; 4] {
    let bytes = x.to_le_bytes();
    let mut out = [0i32; 4];
    for (i, chunk) in bytes.chunks_exact(4).enumerate() {
        out[i] = i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
    }
    out
}

fn from_i32x4(v: [i32; 4]) -> u128 {
    let mut out = [0u8; 16];
    for (i, lane) in v.into_iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&lane.to_le_bytes());
    }
    u128::from_le_bytes(out)
}

fn from_i64x2(v: [i64; 2]) -> u128 {
    let mut out = [0u8; 16];
    for (i, lane) in v.into_iter().enumerate() {
        out[i * 8..i * 8 + 8].copy_from_slice(&lane.to_le_bytes());
    }
    u128::from_le_bytes(out)
}

fn as_u32x4(x: u128) -> [u32; 4] {
    let bytes = x.to_le_bytes();
    let mut out = [0u32; 4];
    for (i, chunk) in bytes.chunks_exact(4).enumerate() {
        out[i] = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
    }
    out
}

fn from_u32x4(v: [u32; 4]) -> u128 {
    let mut out = [0u8; 16];
    for (i, lane) in v.into_iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&lane.to_le_bytes());
    }
    u128::from_le_bytes(out)
}

pub fn pmulld(dst: u128, src: u128) -> u128 {
    let a = as_i32x4(dst);
    let b = as_i32x4(src);
    let out = [
        a[0].wrapping_mul(b[0]),
        a[1].wrapping_mul(b[1]),
        a[2].wrapping_mul(b[2]),
        a[3].wrapping_mul(b[3]),
    ];
    from_i32x4(out)
}

pub fn pblendw(dst: u128, src: u128, imm8: u8) -> u128 {
    let dst = dst.to_le_bytes();
    let src = src.to_le_bytes();
    let mut out = dst;
    for i in 0..8 {
        if (imm8 >> i) & 1 != 0 {
            out[i * 2..i * 2 + 2].copy_from_slice(&src[i * 2..i * 2 + 2]);
        }
    }
    u128::from_le_bytes(out)
}

pub fn ptest(a: u128, b: u128) -> (bool, bool) {
    let a = a.to_le_bytes();
    let b = b.to_le_bytes();
    let mut and_any = 0u8;
    let mut andn_any = 0u8;
    for i in 0..16 {
        and_any |= a[i] & b[i];
        andn_any |= (!a[i]) & b[i];
    }
    (and_any == 0, andn_any == 0)
}

pub fn pmovsxbd(src: &[u8]) -> u128 {
    let mut out = [0i32; 4];
    for i in 0..4 {
        out[i] = (src[i] as i8) as i32;
    }
    from_i32x4(out)
}

pub fn pmovsxbw(src: &[u8]) -> u128 {
    let mut out = [0i16; 8];
    for i in 0..8 {
        out[i] = (src[i] as i8) as i16;
    }
    from_i16x8(out)
}

pub fn pmovsxbq(src: &[u8]) -> u128 {
    from_i64x2([(src[0] as i8) as i64, (src[1] as i8) as i64])
}

pub fn pmovzxbd(src: &[u8]) -> u128 {
    let mut out = [0i32; 4];
    for i in 0..4 {
        out[i] = src[i] as i32;
    }
    from_i32x4(out)
}

pub fn pmovzxbw(src: &[u8]) -> u128 {
    let mut out = [0i16; 8];
    for i in 0..8 {
        out[i] = src[i] as i16;
    }
    from_i16x8(out)
}

pub fn pmovzxbq(src: &[u8]) -> u128 {
    from_i64x2([src[0] as i64, src[1] as i64])
}

pub fn insertps(dst: u128, src: u128, imm8: u8) -> u128 {
    let src_sel = ((imm8 >> 6) & 0x3) as usize;
    let dst_sel = ((imm8 >> 4) & 0x3) as usize;
    let zmask = imm8 & 0x0F;

    let src_lanes = as_u32x4(src);
    let mut dst_lanes = as_u32x4(dst);

    dst_lanes[dst_sel] = src_lanes[src_sel];
    for i in 0..4 {
        if (zmask >> i) & 1 != 0 {
            dst_lanes[i] = 0;
        }
    }

    from_u32x4(dst_lanes)
}
