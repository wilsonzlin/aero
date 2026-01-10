fn as_i16x8(x: u128) -> [i16; 8] {
    let bytes = x.to_le_bytes();
    let mut out = [0i16; 8];
    for (i, chunk) in bytes.chunks_exact(2).enumerate() {
        out[i] = i16::from_le_bytes([chunk[0], chunk[1]]);
    }
    out
}

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

pub fn pshufb(dst: u128, mask: u128) -> u128 {
    let dst = dst.to_le_bytes();
    let mask = mask.to_le_bytes();
    let mut out = [0u8; 16];
    for i in 0..16 {
        let m = mask[i];
        if m & 0x80 != 0 {
            out[i] = 0;
        } else {
            out[i] = dst[(m & 0x0F) as usize];
        }
    }
    u128::from_le_bytes(out)
}

pub fn phaddw(dst: u128, src: u128) -> u128 {
    let a = as_i16x8(dst);
    let b = as_i16x8(src);
    let out = [
        a[0].wrapping_add(a[1]),
        a[2].wrapping_add(a[3]),
        a[4].wrapping_add(a[5]),
        a[6].wrapping_add(a[7]),
        b[0].wrapping_add(b[1]),
        b[2].wrapping_add(b[3]),
        b[4].wrapping_add(b[5]),
        b[6].wrapping_add(b[7]),
    ];
    from_i16x8(out)
}

pub fn phaddd(dst: u128, src: u128) -> u128 {
    let a = as_i32x4(dst);
    let b = as_i32x4(src);
    let out = [
        a[0].wrapping_add(a[1]),
        a[2].wrapping_add(a[3]),
        b[0].wrapping_add(b[1]),
        b[2].wrapping_add(b[3]),
    ];
    from_i32x4(out)
}

pub fn phaddsw(dst: u128, src: u128) -> u128 {
    let a = as_i16x8(dst);
    let b = as_i16x8(src);

    fn sat_add(x: i16, y: i16) -> i16 {
        let sum = x as i32 + y as i32;
        sum.clamp(i16::MIN as i32, i16::MAX as i32) as i16
    }

    let out = [
        sat_add(a[0], a[1]),
        sat_add(a[2], a[3]),
        sat_add(a[4], a[5]),
        sat_add(a[6], a[7]),
        sat_add(b[0], b[1]),
        sat_add(b[2], b[3]),
        sat_add(b[4], b[5]),
        sat_add(b[6], b[7]),
    ];
    from_i16x8(out)
}

pub fn pmaddubsw(dst: u128, src: u128) -> u128 {
    let dst = dst.to_le_bytes();
    let src = src.to_le_bytes();
    let mut out_lanes = [0i16; 8];
    for i in 0..8 {
        let a0 = dst[i * 2] as i32;
        let a1 = dst[i * 2 + 1] as i32;
        let b0 = (src[i * 2] as i8) as i32;
        let b1 = (src[i * 2 + 1] as i8) as i32;

        let sum = a0 * b0 + a1 * b1;
        out_lanes[i] = sum.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
    }
    from_i16x8(out_lanes)
}

pub fn pabsb(src: u128) -> u128 {
    let src = src.to_le_bytes();
    let mut out = [0u8; 16];
    for i in 0..16 {
        out[i] = (src[i] as i8).wrapping_abs() as u8;
    }
    u128::from_le_bytes(out)
}

pub fn pabsw(src: u128) -> u128 {
    let lanes = as_i16x8(src);
    from_i16x8(lanes.map(|v| v.wrapping_abs()))
}

pub fn pabsd(src: u128) -> u128 {
    let lanes = as_i32x4(src);
    from_i32x4(lanes.map(|v| v.wrapping_abs()))
}

pub fn palignr(dst: u128, src: u128, imm: u8) -> u128 {
    let shift = imm as usize;
    let dst = dst.to_le_bytes();
    let src = src.to_le_bytes();
    let mut concat = [0u8; 32];
    concat[..16].copy_from_slice(&dst);
    concat[16..].copy_from_slice(&src);

    let mut out = [0u8; 16];
    for i in 0..16 {
        out[i] = concat.get(i + shift).copied().unwrap_or(0);
    }
    u128::from_le_bytes(out)
}

