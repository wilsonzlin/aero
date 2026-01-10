const CRC32C_POLY_REV: u32 = 0x82F63B78;

fn crc32c_update_byte(mut crc: u32, byte: u8) -> u32 {
    crc ^= byte as u32;
    for _ in 0..8 {
        if crc & 1 != 0 {
            crc = (crc >> 1) ^ CRC32C_POLY_REV;
        } else {
            crc >>= 1;
        }
    }
    crc
}

pub fn crc32_u8(seed: u32, val: u8) -> u32 {
    crc32c_update_byte(seed, val)
}

pub fn crc32_u16(seed: u32, val: u16) -> u32 {
    let mut crc = seed;
    for b in val.to_le_bytes() {
        crc = crc32c_update_byte(crc, b);
    }
    crc
}

pub fn crc32_u32(seed: u32, val: u32) -> u32 {
    let mut crc = seed;
    for b in val.to_le_bytes() {
        crc = crc32c_update_byte(crc, b);
    }
    crc
}

pub fn crc32_u64(seed: u32, val: u64) -> u32 {
    let mut crc = seed;
    for b in val.to_le_bytes() {
        crc = crc32c_update_byte(crc, b);
    }
    crc
}

#[derive(Clone, Copy, Debug, Default)]
pub struct PcmpFlags {
    pub cf: bool,
    pub zf: bool,
    pub sf: bool,
    pub of: bool,
}

#[derive(Clone, Copy, Debug)]
enum StrElem {
    Byte,
    Word,
}

#[derive(Clone, Copy, Debug)]
enum StrSignedness {
    Unsigned,
    Signed,
}

#[derive(Clone, Copy, Debug)]
enum StrOp {
    EqualAny,
    Ranges,
    EqualEach,
    EqualOrdered,
}

#[derive(Clone, Copy, Debug)]
enum StrPolarity {
    Positive,
    Negative,
    MaskedPositive,
    MaskedNegative,
}

fn decode_imm(imm8: u8) -> (StrElem, StrSignedness, StrOp, StrPolarity) {
    let (elem, signedness) = match imm8 & 0x3 {
        0x0 => (StrElem::Byte, StrSignedness::Unsigned),
        0x1 => (StrElem::Word, StrSignedness::Unsigned),
        0x2 => (StrElem::Byte, StrSignedness::Signed),
        _ => (StrElem::Word, StrSignedness::Signed),
    };

    let op = match (imm8 >> 2) & 0x3 {
        0x0 => StrOp::EqualAny,
        0x1 => StrOp::Ranges,
        0x2 => StrOp::EqualEach,
        _ => StrOp::EqualOrdered,
    };

    let polarity = match (imm8 >> 4) & 0x3 {
        0x0 => StrPolarity::Positive,
        0x1 => StrPolarity::Negative,
        0x2 => StrPolarity::MaskedPositive,
        _ => StrPolarity::MaskedNegative,
    };

    (elem, signedness, op, polarity)
}

fn extract_elements(x: u128, elem: StrElem, signedness: StrSignedness) -> Vec<i32> {
    let bytes = x.to_le_bytes();
    match elem {
        StrElem::Byte => bytes
            .iter()
            .map(|&b| match signedness {
                StrSignedness::Unsigned => b as i32,
                StrSignedness::Signed => (b as i8) as i32,
            })
            .collect(),
        StrElem::Word => bytes
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .map(|w| match signedness {
                StrSignedness::Unsigned => w as i32,
                StrSignedness::Signed => (w as i16) as i32,
            })
            .collect(),
    }
}

fn implicit_len(elements: &[i32]) -> usize {
    elements
        .iter()
        .position(|&v| v == 0)
        .unwrap_or(elements.len())
}

fn pcmp_generate_mask(
    a: u128,
    b: u128,
    imm8: u8,
    explicit_lens: Option<(u32, u32)>,
) -> (u32, usize, usize, usize, PcmpFlags) {
    let (elem, signedness, op, polarity) = decode_imm(imm8);
    let a_elems = extract_elements(a, elem, signedness);
    let b_elems = extract_elements(b, elem, signedness);
    let n = a_elems.len(); // 16 bytes or 8 words

    let (len_a, len_b) = if let Some((la, lb)) = explicit_lens {
        (usize::min(la as usize, n), usize::min(lb as usize, n))
    } else {
        (implicit_len(&a_elems), implicit_len(&b_elems))
    };

    let mut res1 = 0u32;
    match op {
        StrOp::EqualAny => {
            for i in 0..len_a {
                let ai = a_elems[i];
                if b_elems[..len_b].iter().any(|&bj| bj == ai) {
                    res1 |= 1u32 << i;
                }
            }
        }
        StrOp::Ranges => {
            let range_count = len_b / 2;
            for i in 0..len_a {
                let ai = a_elems[i];
                let mut match_any = false;
                for r in 0..range_count {
                    let lo = b_elems[r * 2];
                    let hi = b_elems[r * 2 + 1];
                    let (minv, maxv) = if lo <= hi { (lo, hi) } else { (hi, lo) };
                    if ai >= minv && ai <= maxv {
                        match_any = true;
                        break;
                    }
                }
                if match_any {
                    res1 |= 1u32 << i;
                }
            }
        }
        StrOp::EqualEach => {
            let common = usize::min(len_a, len_b);
            for i in 0..common {
                if a_elems[i] == b_elems[i] {
                    res1 |= 1u32 << i;
                }
            }
        }
        StrOp::EqualOrdered => {
            if len_b == 0 {
                res1 |= 1u32;
            } else if len_b <= len_a {
                for i in 0..=(len_a - len_b) {
                    if (0..len_b).all(|j| a_elems[i + j] == b_elems[j]) {
                        res1 |= 1u32 << i;
                    }
                }
            }
        }
    }

    let full_mask = (1u32 << n) - 1;
    let valid_mask = if len_a >= n {
        full_mask
    } else if len_a == 0 {
        0
    } else {
        (1u32 << len_a) - 1
    };

    let res2 = match polarity {
        StrPolarity::Positive => res1,
        StrPolarity::Negative => (!res1) & full_mask,
        StrPolarity::MaskedPositive => res1 & valid_mask,
        StrPolarity::MaskedNegative => (!res1) & valid_mask,
    };

    let flags = PcmpFlags {
        cf: res2 != 0,
        zf: len_b < n,
        sf: len_a < n,
        of: false,
    };

    (res2, n, len_a, len_b, flags)
}

fn pcmp_index(mask: u32, n: usize, imm8: u8) -> u32 {
    if mask == 0 {
        return n as u32;
    }
    if (imm8 & 0x40) == 0 {
        mask.trailing_zeros()
    } else {
        31 - mask.leading_zeros()
    }
}

fn pcmp_mask_to_xmm(mask: u32, n: usize, imm8: u8, elem: StrElem) -> u128 {
    let unit_mask = (imm8 & 0x40) != 0;
    let mut out = [0u8; 16];

    if !unit_mask {
        out[..2].copy_from_slice(&(mask as u16).to_le_bytes());
        return u128::from_le_bytes(out);
    }

    match elem {
        StrElem::Byte => {
            for i in 0..n {
                out[i] = if ((mask >> i) & 1) != 0 { 0xFF } else { 0 };
            }
        }
        StrElem::Word => {
            for i in 0..n {
                let v = if ((mask >> i) & 1) != 0 { 0xFFFFu16 } else { 0 };
                out[i * 2..i * 2 + 2].copy_from_slice(&v.to_le_bytes());
            }
        }
    }

    u128::from_le_bytes(out)
}

pub fn pcmpi_stri(a: u128, b: u128, imm8: u8) -> (u32, PcmpFlags) {
    let (mask, n, _len_a, _len_b, flags) = pcmp_generate_mask(a, b, imm8, None);
    (pcmp_index(mask, n, imm8), flags)
}

pub fn pcmpi_strm(a: u128, b: u128, imm8: u8) -> (u128, PcmpFlags) {
    let (elem, signedness, _op, _polarity) = decode_imm(imm8);
    let _ = signedness;
    let (mask, n, _len_a, _len_b, flags) = pcmp_generate_mask(a, b, imm8, None);
    (pcmp_mask_to_xmm(mask, n, imm8, elem), flags)
}

pub fn pcmpe_stri(a: u128, b: u128, imm8: u8, len_a: u32, len_b: u32) -> (u32, PcmpFlags) {
    let (mask, n, _len_a, _len_b, flags) = pcmp_generate_mask(a, b, imm8, Some((len_a, len_b)));
    (pcmp_index(mask, n, imm8), flags)
}

pub fn pcmpe_strm(a: u128, b: u128, imm8: u8, len_a: u32, len_b: u32) -> (u128, PcmpFlags) {
    let (elem, signedness, _op, _polarity) = decode_imm(imm8);
    let _ = signedness;
    let (mask, n, _len_a, _len_b, flags) = pcmp_generate_mask(a, b, imm8, Some((len_a, len_b)));
    (pcmp_mask_to_xmm(mask, n, imm8, elem), flags)
}

