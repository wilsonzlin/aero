use crate::{CpuState, Exception};
use crate::bus::Bus;

use super::{
    check_alignment, or_mxcsr_flags, read_xmm_operand_128, read_xmm_operand_u64, rounding_mode,
    u128_set_low_u64_preserve, RoundingMode, XmmOperand, XmmReg, MXCSR_IE, MXCSR_PE,
};

type Result<T> = core::result::Result<T, Exception>;

fn u128_to_bytes(v: u128) -> [u8; 16] {
    v.to_le_bytes()
}

fn bytes_to_u128(v: [u8; 16]) -> u128 {
    u128::from_le_bytes(v)
}

fn u128_to_u16x8(v: u128) -> [u16; 8] {
    let bytes = u128_to_bytes(v);
    let mut out = [0u16; 8];
    for (i, chunk) in bytes.chunks_exact(2).enumerate() {
        out[i] = u16::from_le_bytes(chunk.try_into().unwrap());
    }
    out
}

fn u16x8_to_u128(v: [u16; 8]) -> u128 {
    let mut bytes = [0u8; 16];
    for (i, lane) in v.iter().copied().enumerate() {
        bytes[i * 2..i * 2 + 2].copy_from_slice(&lane.to_le_bytes());
    }
    bytes_to_u128(bytes)
}

fn u128_to_u32x4(v: u128) -> [u32; 4] {
    let bytes = u128_to_bytes(v);
    let mut out = [0u32; 4];
    for (i, chunk) in bytes.chunks_exact(4).enumerate() {
        out[i] = u32::from_le_bytes(chunk.try_into().unwrap());
    }
    out
}

fn u32x4_to_u128(v: [u32; 4]) -> u128 {
    let mut bytes = [0u8; 16];
    for (i, lane) in v.iter().copied().enumerate() {
        bytes[i * 4..i * 4 + 4].copy_from_slice(&lane.to_le_bytes());
    }
    bytes_to_u128(bytes)
}

fn u128_to_u64x2(v: u128) -> [u64; 2] {
    let bytes = u128_to_bytes(v);
    let mut out = [0u64; 2];
    for (i, chunk) in bytes.chunks_exact(8).enumerate() {
        out[i] = u64::from_le_bytes(chunk.try_into().unwrap());
    }
    out
}

fn u64x2_to_u128(v: [u64; 2]) -> u128 {
    let mut bytes = [0u8; 16];
    for (i, lane) in v.iter().copied().enumerate() {
        bytes[i * 8..i * 8 + 8].copy_from_slice(&lane.to_le_bytes());
    }
    bytes_to_u128(bytes)
}

fn u128_to_f64x2(v: u128) -> [f64; 2] {
    let lanes = u128_to_u64x2(v);
    lanes.map(f64::from_bits)
}

fn f64x2_to_u128(v: [f64; 2]) -> u128 {
    let lanes = v.map(f64::to_bits);
    u64x2_to_u128(lanes)
}

fn apply_rounding_mode_f64(val: f64, mode: RoundingMode) -> f64 {
    match mode {
        RoundingMode::Nearest => val.round_ties_even(),
        RoundingMode::Down => val.floor(),
        RoundingMode::Up => val.ceil(),
        RoundingMode::TowardZero => val.trunc(),
    }
}

fn cvt_i64_to_f64(cpu: &mut CpuState, src: i64) -> f64 {
    if src == 0 {
        return 0.0;
    }

    let sign = src < 0;
    let mag: u64 = if sign {
        src.wrapping_neg() as u64
    } else {
        src as u64
    };

    let msb = 63 - mag.leading_zeros();
    let mut exp = (msb as i32) + 1023;
    let shift = (msb as i32) - 52;

    let (mut mantissa_full, rem) = if shift <= 0 {
        ((mag << (-shift as u32)) as u64, 0u64)
    } else {
        let shift = shift as u32;
        ((mag >> shift) as u64, mag & ((1u64 << shift) - 1))
    };

    if rem != 0 {
        or_mxcsr_flags(cpu, MXCSR_PE);
    }

    let inc = if rem == 0 {
        false
    } else {
        match rounding_mode(cpu.sse.mxcsr) {
            RoundingMode::Nearest => {
                let shift = shift as u32;
                let half = 1u64 << (shift - 1);
                rem > half || (rem == half && (mantissa_full & 1) == 1)
            }
            RoundingMode::Down => sign,
            RoundingMode::Up => !sign,
            RoundingMode::TowardZero => false,
        }
    };

    if inc {
        mantissa_full = mantissa_full.wrapping_add(1);
        if mantissa_full == (1u64 << 53) {
            mantissa_full >>= 1;
            exp += 1;
        }
    }

    let mantissa = mantissa_full & ((1u64 << 52) - 1);
    let bits = ((sign as u64) << 63) | ((exp as u64) << 52) | mantissa;
    f64::from_bits(bits)
}

fn cvt_float_to_i32(cpu: &mut CpuState, val: f64, truncate: bool) -> i32 {
    if !val.is_finite() || val.is_nan() {
        or_mxcsr_flags(cpu, MXCSR_IE);
        return i32::MIN;
    }

    let rounded = if truncate {
        val.trunc()
    } else {
        apply_rounding_mode_f64(val, rounding_mode(cpu.sse.mxcsr))
    };

    if rounded < (i32::MIN as f64) || rounded > (i32::MAX as f64) {
        or_mxcsr_flags(cpu, MXCSR_IE);
        return i32::MIN;
    }

    if val != rounded {
        or_mxcsr_flags(cpu, MXCSR_PE);
    }

    rounded as i32
}

fn cvt_float_to_i64(cpu: &mut CpuState, val: f64, truncate: bool) -> i64 {
    if !val.is_finite() || val.is_nan() {
        or_mxcsr_flags(cpu, MXCSR_IE);
        return i64::MIN;
    }

    let rounded = if truncate {
        val.trunc()
    } else {
        apply_rounding_mode_f64(val, rounding_mode(cpu.sse.mxcsr))
    };

    if rounded < (i64::MIN as f64) || rounded > (i64::MAX as f64) {
        or_mxcsr_flags(cpu, MXCSR_IE);
        return i64::MIN;
    }

    if val != rounded {
        or_mxcsr_flags(cpu, MXCSR_PE);
    }

    rounded as i64
}

/// MOVDQA: aligned 128-bit integer move.
pub fn movdqa(
    cpu: &mut CpuState,
    bus: &mut impl Bus,
    dst: XmmOperand,
    src: XmmOperand,
    align_check: bool,
) -> Result<()> {
    mov128(cpu, bus, dst, src, align_check)
}

/// MOVDQU: unaligned 128-bit integer move.
pub fn movdqu(
    cpu: &mut CpuState,
    bus: &mut impl Bus,
    dst: XmmOperand,
    src: XmmOperand,
) -> Result<()> {
    mov128(cpu, bus, dst, src, false)
}

fn mov128(
    cpu: &mut CpuState,
    bus: &mut impl Bus,
    dst: XmmOperand,
    src: XmmOperand,
    align_check: bool,
) -> Result<()> {
    match (dst, src) {
        (XmmOperand::Reg(d), XmmOperand::Reg(s)) => {
            cpu.sse.xmm[d.index()] = cpu.sse.xmm[s.index()];
            Ok(())
        }
        (XmmOperand::Reg(d), XmmOperand::Mem(addr)) => {
            check_alignment(align_check, addr, 16)?;
            cpu.sse.xmm[d.index()] = bus.read_u128(addr);
            Ok(())
        }
        (XmmOperand::Mem(addr), XmmOperand::Reg(s)) => {
            check_alignment(align_check, addr, 16)?;
            bus.write_u128(addr, cpu.sse.xmm[s.index()]);
            Ok(())
        }
        (XmmOperand::Mem(_), XmmOperand::Mem(_)) => Err(Exception::InvalidOpcode),
    }
}

/// MOVSD: move low 64-bits, preserving high 64-bits for register destinations.
pub fn movsd(
    cpu: &mut CpuState,
    bus: &mut impl Bus,
    dst: XmmOperand,
    src: XmmOperand,
) -> Result<()> {
    match (dst, src) {
        (XmmOperand::Reg(d), _) => {
            let src_bits = read_xmm_operand_u64(cpu, bus, src);
            let dst_old = cpu.sse.xmm[d.index()];
            cpu.sse.xmm[d.index()] = u128_set_low_u64_preserve(dst_old, src_bits);
            Ok(())
        }
        (XmmOperand::Mem(addr), XmmOperand::Reg(s)) => {
            bus.write_u64(addr, cpu.sse.xmm[s.index()] as u64);
            Ok(())
        }
        (XmmOperand::Mem(_), XmmOperand::Mem(_)) => Err(Exception::InvalidOpcode),
    }
}

/// MOVD: dst XMM = zero-extended src.
pub fn movd_xmm_from_u32(cpu: &mut CpuState, dst: XmmReg, src: u32) {
    cpu.sse.xmm[dst.index()] = src as u128;
}

pub fn movd_xmm_from_mem(cpu: &mut CpuState, bus: &mut impl Bus, dst: XmmReg, addr: u64) {
    cpu.sse.xmm[dst.index()] = bus.read_u32(addr) as u128;
}

pub fn movd_u32_from_xmm(cpu: &CpuState, src: XmmReg) -> u32 {
    cpu.sse.xmm[src.index()] as u32
}

pub fn movd_mem_from_xmm(cpu: &CpuState, bus: &mut impl Bus, addr: u64, src: XmmReg) {
    bus.write_u32(addr, cpu.sse.xmm[src.index()] as u32);
}

/// MOVQ: dst XMM = zero-extended src.
pub fn movq_xmm_from_u64(cpu: &mut CpuState, dst: XmmReg, src: u64) {
    cpu.sse.xmm[dst.index()] = src as u128;
}

pub fn movq_xmm_from_mem(cpu: &mut CpuState, bus: &mut impl Bus, dst: XmmReg, addr: u64) {
    cpu.sse.xmm[dst.index()] = bus.read_u64(addr) as u128;
}

pub fn movq_u64_from_xmm(cpu: &CpuState, src: XmmReg) -> u64 {
    cpu.sse.xmm[src.index()] as u64
}

pub fn movq_mem_from_xmm(cpu: &CpuState, bus: &mut impl Bus, addr: u64, src: XmmReg) {
    bus.write_u64(addr, cpu.sse.xmm[src.index()] as u64);
}

pub fn pand(
    cpu: &mut CpuState,
    bus: &mut impl Bus,
    dst: XmmReg,
    src: XmmOperand,
) -> Result<()> {
    let s = read_xmm_operand_128(cpu, bus, src);
    cpu.sse.xmm[dst.index()] &= s;
    Ok(())
}

pub fn por(
    cpu: &mut CpuState,
    bus: &mut impl Bus,
    dst: XmmReg,
    src: XmmOperand,
) -> Result<()> {
    let s = read_xmm_operand_128(cpu, bus, src);
    cpu.sse.xmm[dst.index()] |= s;
    Ok(())
}

pub fn pxor(
    cpu: &mut CpuState,
    bus: &mut impl Bus,
    dst: XmmReg,
    src: XmmOperand,
) -> Result<()> {
    let s = read_xmm_operand_128(cpu, bus, src);
    cpu.sse.xmm[dst.index()] ^= s;
    Ok(())
}

pub fn pandn(
    cpu: &mut CpuState,
    bus: &mut impl Bus,
    dst: XmmReg,
    src: XmmOperand,
) -> Result<()> {
    let s = read_xmm_operand_128(cpu, bus, src);
    cpu.sse.xmm[dst.index()] = !cpu.sse.xmm[dst.index()] & s;
    Ok(())
}

pub fn paddb(
    cpu: &mut CpuState,
    bus: &mut impl Bus,
    dst: XmmReg,
    src: XmmOperand,
) -> Result<()> {
    let a = u128_to_bytes(cpu.sse.xmm[dst.index()]);
    let b = u128_to_bytes(read_xmm_operand_128(cpu, bus, src));
    let mut out = [0u8; 16];
    for i in 0..16 {
        out[i] = a[i].wrapping_add(b[i]);
    }
    cpu.sse.xmm[dst.index()] = bytes_to_u128(out);
    Ok(())
}

pub fn paddw(
    cpu: &mut CpuState,
    bus: &mut impl Bus,
    dst: XmmReg,
    src: XmmOperand,
) -> Result<()> {
    let a = u128_to_u16x8(cpu.sse.xmm[dst.index()]);
    let b = u128_to_u16x8(read_xmm_operand_128(cpu, bus, src));
    let mut out = [0u16; 8];
    for i in 0..8 {
        out[i] = a[i].wrapping_add(b[i]);
    }
    cpu.sse.xmm[dst.index()] = u16x8_to_u128(out);
    Ok(())
}

pub fn paddd(
    cpu: &mut CpuState,
    bus: &mut impl Bus,
    dst: XmmReg,
    src: XmmOperand,
) -> Result<()> {
    let a = u128_to_u32x4(cpu.sse.xmm[dst.index()]);
    let b = u128_to_u32x4(read_xmm_operand_128(cpu, bus, src));
    let mut out = [0u32; 4];
    for i in 0..4 {
        out[i] = a[i].wrapping_add(b[i]);
    }
    cpu.sse.xmm[dst.index()] = u32x4_to_u128(out);
    Ok(())
}

pub fn paddq(
    cpu: &mut CpuState,
    bus: &mut impl Bus,
    dst: XmmReg,
    src: XmmOperand,
) -> Result<()> {
    let a = u128_to_u64x2(cpu.sse.xmm[dst.index()]);
    let b = u128_to_u64x2(read_xmm_operand_128(cpu, bus, src));
    cpu.sse.xmm[dst.index()] =
        u64x2_to_u128([a[0].wrapping_add(b[0]), a[1].wrapping_add(b[1])]);
    Ok(())
}

pub fn psubb(
    cpu: &mut CpuState,
    bus: &mut impl Bus,
    dst: XmmReg,
    src: XmmOperand,
) -> Result<()> {
    let a = u128_to_bytes(cpu.sse.xmm[dst.index()]);
    let b = u128_to_bytes(read_xmm_operand_128(cpu, bus, src));
    let mut out = [0u8; 16];
    for i in 0..16 {
        out[i] = a[i].wrapping_sub(b[i]);
    }
    cpu.sse.xmm[dst.index()] = bytes_to_u128(out);
    Ok(())
}

pub fn psubw(
    cpu: &mut CpuState,
    bus: &mut impl Bus,
    dst: XmmReg,
    src: XmmOperand,
) -> Result<()> {
    let a = u128_to_u16x8(cpu.sse.xmm[dst.index()]);
    let b = u128_to_u16x8(read_xmm_operand_128(cpu, bus, src));
    let mut out = [0u16; 8];
    for i in 0..8 {
        out[i] = a[i].wrapping_sub(b[i]);
    }
    cpu.sse.xmm[dst.index()] = u16x8_to_u128(out);
    Ok(())
}

pub fn psubd(
    cpu: &mut CpuState,
    bus: &mut impl Bus,
    dst: XmmReg,
    src: XmmOperand,
) -> Result<()> {
    let a = u128_to_u32x4(cpu.sse.xmm[dst.index()]);
    let b = u128_to_u32x4(read_xmm_operand_128(cpu, bus, src));
    let mut out = [0u32; 4];
    for i in 0..4 {
        out[i] = a[i].wrapping_sub(b[i]);
    }
    cpu.sse.xmm[dst.index()] = u32x4_to_u128(out);
    Ok(())
}

pub fn psubq(
    cpu: &mut CpuState,
    bus: &mut impl Bus,
    dst: XmmReg,
    src: XmmOperand,
) -> Result<()> {
    let a = u128_to_u64x2(cpu.sse.xmm[dst.index()]);
    let b = u128_to_u64x2(read_xmm_operand_128(cpu, bus, src));
    cpu.sse.xmm[dst.index()] =
        u64x2_to_u128([a[0].wrapping_sub(b[0]), a[1].wrapping_sub(b[1])]);
    Ok(())
}

pub fn pmullw(
    cpu: &mut CpuState,
    bus: &mut impl Bus,
    dst: XmmReg,
    src: XmmOperand,
) -> Result<()> {
    let a = u128_to_u16x8(cpu.sse.xmm[dst.index()]);
    let b = u128_to_u16x8(read_xmm_operand_128(cpu, bus, src));
    let mut out = [0u16; 8];
    for i in 0..8 {
        let prod = (a[i] as i16 as i32) * (b[i] as i16 as i32);
        out[i] = (prod as i16) as u16;
    }
    cpu.sse.xmm[dst.index()] = u16x8_to_u128(out);
    Ok(())
}

pub fn pmuludq(
    cpu: &mut CpuState,
    bus: &mut impl Bus,
    dst: XmmReg,
    src: XmmOperand,
) -> Result<()> {
    let a = u128_to_u32x4(cpu.sse.xmm[dst.index()]);
    let b = u128_to_u32x4(read_xmm_operand_128(cpu, bus, src));
    let lo = (a[0] as u64) * (b[0] as u64);
    let hi = (a[2] as u64) * (b[2] as u64);
    cpu.sse.xmm[dst.index()] = u64x2_to_u128([lo, hi]);
    Ok(())
}

pub fn paddsb(
    cpu: &mut CpuState,
    bus: &mut impl Bus,
    dst: XmmReg,
    src: XmmOperand,
) -> Result<()> {
    let a = u128_to_bytes(cpu.sse.xmm[dst.index()]);
    let b = u128_to_bytes(read_xmm_operand_128(cpu, bus, src));
    let mut out = [0u8; 16];
    for i in 0..16 {
        let sum = (a[i] as i8 as i16) + (b[i] as i8 as i16);
        out[i] = sum.clamp(i8::MIN as i16, i8::MAX as i16) as i8 as u8;
    }
    cpu.sse.xmm[dst.index()] = bytes_to_u128(out);
    Ok(())
}

pub fn paddusb(
    cpu: &mut CpuState,
    bus: &mut impl Bus,
    dst: XmmReg,
    src: XmmOperand,
) -> Result<()> {
    let a = u128_to_bytes(cpu.sse.xmm[dst.index()]);
    let b = u128_to_bytes(read_xmm_operand_128(cpu, bus, src));
    let mut out = [0u8; 16];
    for i in 0..16 {
        let sum = (a[i] as u16) + (b[i] as u16);
        out[i] = sum.min(u8::MAX as u16) as u8;
    }
    cpu.sse.xmm[dst.index()] = bytes_to_u128(out);
    Ok(())
}

pub fn psubsb(
    cpu: &mut CpuState,
    bus: &mut impl Bus,
    dst: XmmReg,
    src: XmmOperand,
) -> Result<()> {
    let a = u128_to_bytes(cpu.sse.xmm[dst.index()]);
    let b = u128_to_bytes(read_xmm_operand_128(cpu, bus, src));
    let mut out = [0u8; 16];
    for i in 0..16 {
        let diff = (a[i] as i8 as i16) - (b[i] as i8 as i16);
        out[i] = diff.clamp(i8::MIN as i16, i8::MAX as i16) as i8 as u8;
    }
    cpu.sse.xmm[dst.index()] = bytes_to_u128(out);
    Ok(())
}

pub fn psubusb(
    cpu: &mut CpuState,
    bus: &mut impl Bus,
    dst: XmmReg,
    src: XmmOperand,
) -> Result<()> {
    let a = u128_to_bytes(cpu.sse.xmm[dst.index()]);
    let b = u128_to_bytes(read_xmm_operand_128(cpu, bus, src));
    let mut out = [0u8; 16];
    for i in 0..16 {
        out[i] = a[i].saturating_sub(b[i]);
    }
    cpu.sse.xmm[dst.index()] = bytes_to_u128(out);
    Ok(())
}

pub fn psllw(cpu: &mut CpuState, dst: XmmReg, count: u8) {
    if count > 15 {
        cpu.sse.xmm[dst.index()] = 0;
        return;
    }
    let a = u128_to_u16x8(cpu.sse.xmm[dst.index()]);
    let mut out = [0u16; 8];
    for i in 0..8 {
        out[i] = a[i].wrapping_shl(count as u32);
    }
    cpu.sse.xmm[dst.index()] = u16x8_to_u128(out);
}

pub fn pslld(cpu: &mut CpuState, dst: XmmReg, count: u8) {
    if count > 31 {
        cpu.sse.xmm[dst.index()] = 0;
        return;
    }
    let a = u128_to_u32x4(cpu.sse.xmm[dst.index()]);
    let mut out = [0u32; 4];
    for i in 0..4 {
        out[i] = a[i].wrapping_shl(count as u32);
    }
    cpu.sse.xmm[dst.index()] = u32x4_to_u128(out);
}

pub fn psllq(cpu: &mut CpuState, dst: XmmReg, count: u8) {
    if count > 63 {
        cpu.sse.xmm[dst.index()] = 0;
        return;
    }
    let a = u128_to_u64x2(cpu.sse.xmm[dst.index()]);
    cpu.sse.xmm[dst.index()] =
        u64x2_to_u128([a[0].wrapping_shl(count as u32), a[1].wrapping_shl(count as u32)]);
}

pub fn psrlw(cpu: &mut CpuState, dst: XmmReg, count: u8) {
    if count > 15 {
        cpu.sse.xmm[dst.index()] = 0;
        return;
    }
    let a = u128_to_u16x8(cpu.sse.xmm[dst.index()]);
    let mut out = [0u16; 8];
    for i in 0..8 {
        out[i] = a[i].wrapping_shr(count as u32);
    }
    cpu.sse.xmm[dst.index()] = u16x8_to_u128(out);
}

pub fn psrld(cpu: &mut CpuState, dst: XmmReg, count: u8) {
    if count > 31 {
        cpu.sse.xmm[dst.index()] = 0;
        return;
    }
    let a = u128_to_u32x4(cpu.sse.xmm[dst.index()]);
    let mut out = [0u32; 4];
    for i in 0..4 {
        out[i] = a[i].wrapping_shr(count as u32);
    }
    cpu.sse.xmm[dst.index()] = u32x4_to_u128(out);
}

pub fn psrlq(cpu: &mut CpuState, dst: XmmReg, count: u8) {
    if count > 63 {
        cpu.sse.xmm[dst.index()] = 0;
        return;
    }
    let a = u128_to_u64x2(cpu.sse.xmm[dst.index()]);
    cpu.sse.xmm[dst.index()] =
        u64x2_to_u128([a[0].wrapping_shr(count as u32), a[1].wrapping_shr(count as u32)]);
}

pub fn psraw(cpu: &mut CpuState, dst: XmmReg, count: u8) {
    let count = count.min(15);
    let a = u128_to_u16x8(cpu.sse.xmm[dst.index()]);
    let mut out = [0u16; 8];
    for i in 0..8 {
        out[i] = ((a[i] as i16) >> count) as u16;
    }
    cpu.sse.xmm[dst.index()] = u16x8_to_u128(out);
}

pub fn psrad(cpu: &mut CpuState, dst: XmmReg, count: u8) {
    let count = count.min(31);
    let a = u128_to_u32x4(cpu.sse.xmm[dst.index()]);
    let mut out = [0u32; 4];
    for i in 0..4 {
        out[i] = ((a[i] as i32) >> count) as u32;
    }
    cpu.sse.xmm[dst.index()] = u32x4_to_u128(out);
}

pub fn pslldq(cpu: &mut CpuState, dst: XmmReg, count: u8) {
    if count >= 16 {
        cpu.sse.xmm[dst.index()] = 0;
    } else {
        cpu.sse.xmm[dst.index()] <<= (count as u32) * 8;
    }
}

pub fn psrldq(cpu: &mut CpuState, dst: XmmReg, count: u8) {
    if count >= 16 {
        cpu.sse.xmm[dst.index()] = 0;
    } else {
        cpu.sse.xmm[dst.index()] >>= (count as u32) * 8;
    }
}

pub fn pcmpeqb(
    cpu: &mut CpuState,
    bus: &mut impl Bus,
    dst: XmmReg,
    src: XmmOperand,
) -> Result<()> {
    let a = u128_to_bytes(cpu.sse.xmm[dst.index()]);
    let b = u128_to_bytes(read_xmm_operand_128(cpu, bus, src));
    let mut out = [0u8; 16];
    for i in 0..16 {
        out[i] = if a[i] == b[i] { 0xFF } else { 0 };
    }
    cpu.sse.xmm[dst.index()] = bytes_to_u128(out);
    Ok(())
}

pub fn pcmpeqw(
    cpu: &mut CpuState,
    bus: &mut impl Bus,
    dst: XmmReg,
    src: XmmOperand,
) -> Result<()> {
    let a = u128_to_u16x8(cpu.sse.xmm[dst.index()]);
    let b = u128_to_u16x8(read_xmm_operand_128(cpu, bus, src));
    let mut out = [0u16; 8];
    for i in 0..8 {
        out[i] = if a[i] == b[i] { 0xFFFF } else { 0 };
    }
    cpu.sse.xmm[dst.index()] = u16x8_to_u128(out);
    Ok(())
}

pub fn pcmpeqd(
    cpu: &mut CpuState,
    bus: &mut impl Bus,
    dst: XmmReg,
    src: XmmOperand,
) -> Result<()> {
    let a = u128_to_u32x4(cpu.sse.xmm[dst.index()]);
    let b = u128_to_u32x4(read_xmm_operand_128(cpu, bus, src));
    let mut out = [0u32; 4];
    for i in 0..4 {
        out[i] = if a[i] == b[i] { u32::MAX } else { 0 };
    }
    cpu.sse.xmm[dst.index()] = u32x4_to_u128(out);
    Ok(())
}

pub fn pcmpeqq(
    cpu: &mut CpuState,
    bus: &mut impl Bus,
    dst: XmmReg,
    src: XmmOperand,
) -> Result<()> {
    let a = u128_to_u64x2(cpu.sse.xmm[dst.index()]);
    let b = u128_to_u64x2(read_xmm_operand_128(cpu, bus, src));
    cpu.sse.xmm[dst.index()] = u64x2_to_u128([
        if a[0] == b[0] { u64::MAX } else { 0 },
        if a[1] == b[1] { u64::MAX } else { 0 },
    ]);
    Ok(())
}

pub fn pcmpgtb(
    cpu: &mut CpuState,
    bus: &mut impl Bus,
    dst: XmmReg,
    src: XmmOperand,
) -> Result<()> {
    let a = u128_to_bytes(cpu.sse.xmm[dst.index()]);
    let b = u128_to_bytes(read_xmm_operand_128(cpu, bus, src));
    let mut out = [0u8; 16];
    for i in 0..16 {
        out[i] = if (a[i] as i8) > (b[i] as i8) { 0xFF } else { 0 };
    }
    cpu.sse.xmm[dst.index()] = bytes_to_u128(out);
    Ok(())
}

pub fn pcmpgtw(
    cpu: &mut CpuState,
    bus: &mut impl Bus,
    dst: XmmReg,
    src: XmmOperand,
) -> Result<()> {
    let a = u128_to_u16x8(cpu.sse.xmm[dst.index()]);
    let b = u128_to_u16x8(read_xmm_operand_128(cpu, bus, src));
    let mut out = [0u16; 8];
    for i in 0..8 {
        out[i] = if (a[i] as i16) > (b[i] as i16) { 0xFFFF } else { 0 };
    }
    cpu.sse.xmm[dst.index()] = u16x8_to_u128(out);
    Ok(())
}

pub fn pcmpgtd(
    cpu: &mut CpuState,
    bus: &mut impl Bus,
    dst: XmmReg,
    src: XmmOperand,
) -> Result<()> {
    let a = u128_to_u32x4(cpu.sse.xmm[dst.index()]);
    let b = u128_to_u32x4(read_xmm_operand_128(cpu, bus, src));
    let mut out = [0u32; 4];
    for i in 0..4 {
        out[i] = if (a[i] as i32) > (b[i] as i32) { u32::MAX } else { 0 };
    }
    cpu.sse.xmm[dst.index()] = u32x4_to_u128(out);
    Ok(())
}

pub fn addsd(
    cpu: &mut CpuState,
    bus: &mut impl Bus,
    dst: XmmReg,
    src: XmmOperand,
) -> Result<()> {
    scalar_f64_op(cpu, bus, dst, src, |a, b| a + b)
}

pub fn subsd(
    cpu: &mut CpuState,
    bus: &mut impl Bus,
    dst: XmmReg,
    src: XmmOperand,
) -> Result<()> {
    scalar_f64_op(cpu, bus, dst, src, |a, b| a - b)
}

pub fn mulsd(
    cpu: &mut CpuState,
    bus: &mut impl Bus,
    dst: XmmReg,
    src: XmmOperand,
) -> Result<()> {
    scalar_f64_op(cpu, bus, dst, src, |a, b| a * b)
}

pub fn divsd(
    cpu: &mut CpuState,
    bus: &mut impl Bus,
    dst: XmmReg,
    src: XmmOperand,
) -> Result<()> {
    scalar_f64_op(cpu, bus, dst, src, |a, b| a / b)
}

fn scalar_f64_op(
    cpu: &mut CpuState,
    bus: &mut impl Bus,
    dst: XmmReg,
    src: XmmOperand,
    op: impl FnOnce(f64, f64) -> f64,
) -> Result<()> {
    let a_bits = cpu.sse.xmm[dst.index()] as u64;
    let b_bits = read_xmm_operand_u64(cpu, bus, src);
    let a = f64::from_bits(a_bits);
    let b = f64::from_bits(b_bits);
    let res = op(a, b);
    let dst_old = cpu.sse.xmm[dst.index()];
    cpu.sse.xmm[dst.index()] = u128_set_low_u64_preserve(dst_old, res.to_bits());
    Ok(())
}

pub fn addpd(
    cpu: &mut CpuState,
    bus: &mut impl Bus,
    dst: XmmReg,
    src: XmmOperand,
) -> Result<()> {
    packed_f64_op(cpu, bus, dst, src, |a, b| a + b)
}

pub fn subpd(
    cpu: &mut CpuState,
    bus: &mut impl Bus,
    dst: XmmReg,
    src: XmmOperand,
) -> Result<()> {
    packed_f64_op(cpu, bus, dst, src, |a, b| a - b)
}

pub fn mulpd(
    cpu: &mut CpuState,
    bus: &mut impl Bus,
    dst: XmmReg,
    src: XmmOperand,
) -> Result<()> {
    packed_f64_op(cpu, bus, dst, src, |a, b| a * b)
}

pub fn divpd(
    cpu: &mut CpuState,
    bus: &mut impl Bus,
    dst: XmmReg,
    src: XmmOperand,
) -> Result<()> {
    packed_f64_op(cpu, bus, dst, src, |a, b| a / b)
}

fn packed_f64_op(
    cpu: &mut CpuState,
    bus: &mut impl Bus,
    dst: XmmReg,
    src: XmmOperand,
    op: impl Fn(f64, f64) -> f64,
) -> Result<()> {
    let a = u128_to_f64x2(cpu.sse.xmm[dst.index()]);
    let b = u128_to_f64x2(read_xmm_operand_128(cpu, bus, src));
    cpu.sse.xmm[dst.index()] = f64x2_to_u128([op(a[0], b[0]), op(a[1], b[1])]);
    Ok(())
}

/// CVTSI2SD: dst.low64 = (src as f64), dst.high64 preserved.
pub fn cvtsi2sd(cpu: &mut CpuState, dst: XmmReg, src: i64) {
    let dst_old = cpu.sse.xmm[dst.index()];
    let f = cvt_i64_to_f64(cpu, src);
    cpu.sse.xmm[dst.index()] = u128_set_low_u64_preserve(dst_old, f.to_bits());
}

/// CVTSD2SI (round according to MXCSR.RC).
pub fn cvtsd2si32(cpu: &mut CpuState, bus: &mut impl Bus, src: XmmOperand) -> i32 {
    let bits = read_xmm_operand_u64(cpu, bus, src);
    cvt_float_to_i32(cpu, f64::from_bits(bits), false)
}

pub fn cvtsd2si64(cpu: &mut CpuState, bus: &mut impl Bus, src: XmmOperand) -> i64 {
    let bits = read_xmm_operand_u64(cpu, bus, src);
    cvt_float_to_i64(cpu, f64::from_bits(bits), false)
}

/// CVTTSD2SI (truncate regardless of MXCSR.RC).
pub fn cvttsd2si32(cpu: &mut CpuState, bus: &mut impl Bus, src: XmmOperand) -> i32 {
    let bits = read_xmm_operand_u64(cpu, bus, src);
    cvt_float_to_i32(cpu, f64::from_bits(bits), true)
}

pub fn cvttsd2si64(cpu: &mut CpuState, bus: &mut impl Bus, src: XmmOperand) -> i64 {
    let bits = read_xmm_operand_u64(cpu, bus, src);
    cvt_float_to_i64(cpu, f64::from_bits(bits), true)
}
