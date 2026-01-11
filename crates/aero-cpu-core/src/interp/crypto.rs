//! Software semantics for common x86 crypto SIMD instructions.
//!
//! Tier-0 uses these helpers to implement AES-NI + PCLMULQDQ without depending on
//! platform intrinsics.

/// Carry-less multiply of two 64-bit values in GF(2), returning the full 128-bit product.
#[inline]
fn clmul_u64(a: u64, b: u64) -> u128 {
    let mut acc = 0u128;
    let mut cur = a as u128;
    let mut mask = b;
    for _ in 0..64 {
        if (mask & 1) != 0 {
            acc ^= cur;
        }
        mask >>= 1;
        cur <<= 1;
    }
    acc
}

/// x86 `PCLMULQDQ xmm, xmm/m128, imm8`.
///
/// `imm8[0]` selects the 64-bit lane of `dst` (0=low, 1=high) and `imm8[4]` selects the lane of
/// `src`.
#[inline]
pub fn pclmulqdq(dst: u128, src: u128, imm8: u8) -> u128 {
    let a = if (imm8 & 0x01) == 0 {
        dst as u64
    } else {
        (dst >> 64) as u64
    };
    let b = if (imm8 & 0x10) == 0 {
        src as u64
    } else {
        (src >> 64) as u64
    };
    clmul_u64(a, b)
}

// ---- AES helpers ------------------------------------------------------------

const fn gf_mul_const(mut a: u8, mut b: u8) -> u8 {
    let mut p = 0u8;
    let mut i = 0;
    while i < 8 {
        if (b & 1) != 0 {
            p ^= a;
        }
        let hi = a & 0x80;
        a <<= 1;
        if hi != 0 {
            a ^= 0x1b;
        }
        b >>= 1;
        i += 1;
    }
    p
}

const fn gf_inv(x: u8) -> u8 {
    if x == 0 {
        return 0;
    }
    let mut y: u16 = 1;
    while y < 256 {
        if gf_mul_const(x, y as u8) == 1 {
            return y as u8;
        }
        y += 1;
    }
    0
}

const fn aes_sbox_byte(x: u8) -> u8 {
    let inv = gf_inv(x);
    0x63 ^ inv ^ inv.rotate_left(1) ^ inv.rotate_left(2) ^ inv.rotate_left(3) ^ inv.rotate_left(4)
}

const fn gen_aes_sbox() -> [u8; 256] {
    let mut table = [0u8; 256];
    let mut i = 0usize;
    while i < 256 {
        table[i] = aes_sbox_byte(i as u8);
        i += 1;
    }
    table
}

const fn gen_aes_inv_sbox(sbox: [u8; 256]) -> [u8; 256] {
    let mut table = [0u8; 256];
    let mut i = 0usize;
    while i < 256 {
        table[sbox[i] as usize] = i as u8;
        i += 1;
    }
    table
}

// AES S-box (FIPS-197), derived algorithmically to avoid transcription errors.
const AES_SBOX: [u8; 256] = gen_aes_sbox();
const AES_INV_SBOX: [u8; 256] = gen_aes_inv_sbox(AES_SBOX);

#[inline]
fn sub_bytes(state: &mut [u8; 16]) {
    for b in state.iter_mut() {
        *b = AES_SBOX[*b as usize];
    }
}

#[inline]
fn inv_sub_bytes(state: &mut [u8; 16]) {
    for b in state.iter_mut() {
        *b = AES_INV_SBOX[*b as usize];
    }
}

#[inline]
fn shift_rows(state: &[u8; 16]) -> [u8; 16] {
    let mut out = [0u8; 16];

    // Row 0 (no shift)
    out[0] = state[0];
    out[4] = state[4];
    out[8] = state[8];
    out[12] = state[12];

    // Row 1 (shift left by 1)
    out[1] = state[5];
    out[5] = state[9];
    out[9] = state[13];
    out[13] = state[1];

    // Row 2 (shift left by 2)
    out[2] = state[10];
    out[6] = state[14];
    out[10] = state[2];
    out[14] = state[6];

    // Row 3 (shift left by 3)
    out[3] = state[15];
    out[7] = state[3];
    out[11] = state[7];
    out[15] = state[11];

    out
}

#[inline]
fn inv_shift_rows(state: &[u8; 16]) -> [u8; 16] {
    let mut out = [0u8; 16];

    // Row 0 (no shift)
    out[0] = state[0];
    out[4] = state[4];
    out[8] = state[8];
    out[12] = state[12];

    // Row 1 (shift right by 1)
    out[1] = state[13];
    out[5] = state[1];
    out[9] = state[5];
    out[13] = state[9];

    // Row 2 (shift right by 2 == shift left by 2)
    out[2] = state[10];
    out[6] = state[14];
    out[10] = state[2];
    out[14] = state[6];

    // Row 3 (shift right by 3 == shift left by 1)
    out[3] = state[7];
    out[7] = state[11];
    out[11] = state[15];
    out[15] = state[3];

    out
}

#[inline]
fn gf_mul(mut a: u8, mut b: u8) -> u8 {
    // "Russian peasant multiplication" in GF(2^8) with the AES polynomial 0x11B.
    let mut p = 0u8;
    for _ in 0..8 {
        if (b & 1) != 0 {
            p ^= a;
        }
        let hi = a & 0x80;
        a <<= 1;
        if hi != 0 {
            a ^= 0x1b;
        }
        b >>= 1;
    }
    p
}

#[inline]
fn mix_columns(state: &mut [u8; 16]) {
    for col in 0..4 {
        let i = col * 4;
        let a0 = state[i];
        let a1 = state[i + 1];
        let a2 = state[i + 2];
        let a3 = state[i + 3];

        state[i] = gf_mul(a0, 2) ^ gf_mul(a1, 3) ^ a2 ^ a3;
        state[i + 1] = a0 ^ gf_mul(a1, 2) ^ gf_mul(a2, 3) ^ a3;
        state[i + 2] = a0 ^ a1 ^ gf_mul(a2, 2) ^ gf_mul(a3, 3);
        state[i + 3] = gf_mul(a0, 3) ^ a1 ^ a2 ^ gf_mul(a3, 2);
    }
}

#[inline]
fn inv_mix_columns(state: &mut [u8; 16]) {
    for col in 0..4 {
        let i = col * 4;
        let a0 = state[i];
        let a1 = state[i + 1];
        let a2 = state[i + 2];
        let a3 = state[i + 3];

        state[i] = gf_mul(a0, 0x0e) ^ gf_mul(a1, 0x0b) ^ gf_mul(a2, 0x0d) ^ gf_mul(a3, 0x09);
        state[i + 1] = gf_mul(a0, 0x09) ^ gf_mul(a1, 0x0e) ^ gf_mul(a2, 0x0b) ^ gf_mul(a3, 0x0d);
        state[i + 2] = gf_mul(a0, 0x0d) ^ gf_mul(a1, 0x09) ^ gf_mul(a2, 0x0e) ^ gf_mul(a3, 0x0b);
        state[i + 3] = gf_mul(a0, 0x0b) ^ gf_mul(a1, 0x0d) ^ gf_mul(a2, 0x09) ^ gf_mul(a3, 0x0e);
    }
}

/// x86 `AESENC xmm, xmm/m128`.
#[inline]
pub fn aesenc(state: u128, round_key: u128) -> u128 {
    let mut s = state.to_le_bytes();
    sub_bytes(&mut s);
    s = shift_rows(&s);
    mix_columns(&mut s);
    u128::from_le_bytes(s) ^ round_key
}

/// x86 `AESENCLAST xmm, xmm/m128`.
#[inline]
pub fn aesenclast(state: u128, round_key: u128) -> u128 {
    let mut s = state.to_le_bytes();
    sub_bytes(&mut s);
    s = shift_rows(&s);
    u128::from_le_bytes(s) ^ round_key
}

/// x86 `AESDEC xmm, xmm/m128`.
#[inline]
pub fn aesdec(state: u128, round_key: u128) -> u128 {
    let mut s = state.to_le_bytes();
    s = inv_shift_rows(&s);
    inv_sub_bytes(&mut s);
    inv_mix_columns(&mut s);
    u128::from_le_bytes(s) ^ round_key
}

/// x86 `AESDECLAST xmm, xmm/m128`.
#[inline]
pub fn aesdeclast(state: u128, round_key: u128) -> u128 {
    let mut s = state.to_le_bytes();
    s = inv_shift_rows(&s);
    inv_sub_bytes(&mut s);
    u128::from_le_bytes(s) ^ round_key
}

/// x86 `AESIMC xmm, xmm/m128`.
#[inline]
pub fn aesimc(src: u128) -> u128 {
    let mut s = src.to_le_bytes();
    inv_mix_columns(&mut s);
    u128::from_le_bytes(s)
}

/// x86 `AESKEYGENASSIST xmm, xmm/m128, imm8`.
#[inline]
pub fn aeskeygenassist(src: u128, imm8: u8) -> u128 {
    let bytes = src.to_le_bytes();
    let w1 = [bytes[4], bytes[5], bytes[6], bytes[7]];
    let w3 = [bytes[12], bytes[13], bytes[14], bytes[15]];

    let mut sub1 = [0u8; 4];
    let mut sub3 = [0u8; 4];
    for i in 0..4 {
        sub1[i] = AES_SBOX[w1[i] as usize];
        sub3[i] = AES_SBOX[w3[i] as usize];
    }

    let mut rot1 = [sub1[1], sub1[2], sub1[3], sub1[0]];
    let mut rot3 = [sub3[1], sub3[2], sub3[3], sub3[0]];
    rot1[0] ^= imm8;
    rot3[0] ^= imm8;

    let mut out = [0u8; 16];
    out[0..4].copy_from_slice(&sub1);
    out[4..8].copy_from_slice(&rot1);
    out[8..12].copy_from_slice(&sub3);
    out[12..16].copy_from_slice(&rot3);
    u128::from_le_bytes(out)
}
