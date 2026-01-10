use crate::{CpuState, Exception};
use crate::bus::Bus;

use super::{
    check_alignment, or_mxcsr_flags, read_xmm_operand_128, read_xmm_operand_u32, rounding_mode,
    u128_set_low_u32_preserve, RoundingMode, XmmOperand, XmmReg, MXCSR_IE, MXCSR_PE,
};

type Result<T> = core::result::Result<T, Exception>;

fn u128_to_u32x4(v: u128) -> [u32; 4] {
    let bytes = v.to_le_bytes();
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
    u128::from_le_bytes(bytes)
}

fn u128_to_f32x4(v: u128) -> [f32; 4] {
    let lanes = u128_to_u32x4(v);
    lanes.map(f32::from_bits)
}

fn f32x4_to_u128(v: [f32; 4]) -> u128 {
    let lanes = v.map(f32::to_bits);
    u32x4_to_u128(lanes)
}

fn apply_rounding_mode_f64(val: f64, mode: RoundingMode) -> f64 {
    match mode {
        RoundingMode::Nearest => val.round_ties_even(),
        RoundingMode::Down => val.floor(),
        RoundingMode::Up => val.ceil(),
        RoundingMode::TowardZero => val.trunc(),
    }
}

fn cvt_i64_to_f32(cpu: &mut CpuState, src: i64) -> f32 {
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
    let mut exp = (msb as i32) + 127;
    let shift = (msb as i32) - 23;

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
        if mantissa_full == (1u64 << 24) {
            mantissa_full >>= 1;
            exp += 1;
        }
    }

    let mantissa = (mantissa_full & ((1u64 << 23) - 1)) as u32;
    let bits = ((sign as u32) << 31) | ((exp as u32) << 23) | mantissa;
    f32::from_bits(bits)
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

/// MOVAPS: aligned 128-bit move.
pub fn movaps(
    cpu: &mut CpuState,
    bus: &mut impl Bus,
    dst: XmmOperand,
    src: XmmOperand,
    align_check: bool,
) -> Result<()> {
    mov128(cpu, bus, dst, src, align_check)
}

/// MOVUPS: unaligned 128-bit move.
pub fn movups(
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

/// MOVSS: move low 32-bits, preserving high 96-bits for register destinations.
pub fn movss(
    cpu: &mut CpuState,
    bus: &mut impl Bus,
    dst: XmmOperand,
    src: XmmOperand,
) -> Result<()> {
    match (dst, src) {
        (XmmOperand::Reg(d), _) => {
            let src_bits = read_xmm_operand_u32(cpu, bus, src);
            let dst_old = cpu.sse.xmm[d.index()];
            cpu.sse.xmm[d.index()] = u128_set_low_u32_preserve(dst_old, src_bits);
            Ok(())
        }
        (XmmOperand::Mem(addr), XmmOperand::Reg(s)) => {
            bus.write_u32(addr, cpu.sse.xmm[s.index()] as u32);
            Ok(())
        }
        (XmmOperand::Mem(_), XmmOperand::Mem(_)) => Err(Exception::InvalidOpcode),
    }
}

/// MOVHLPS: dst.low64 = src.high64, dst.high64 unchanged.
pub fn movhlps(cpu: &mut CpuState, dst: XmmReg, src: XmmReg) {
    let dst_old = cpu.sse.xmm[dst.index()];
    let src_val = cpu.sse.xmm[src.index()];
    let dst_high = (dst_old >> 64) as u64;
    let src_high = (src_val >> 64) as u64;
    cpu.sse.xmm[dst.index()] = ((dst_high as u128) << 64) | (src_high as u128);
}

/// MOVLHPS: dst.high64 = src.low64, dst.low64 unchanged.
pub fn movlhps(cpu: &mut CpuState, dst: XmmReg, src: XmmReg) {
    let dst_old = cpu.sse.xmm[dst.index()];
    let src_val = cpu.sse.xmm[src.index()];
    let dst_low = dst_old as u64;
    let src_low = src_val as u64;
    cpu.sse.xmm[dst.index()] = ((src_low as u128) << 64) | (dst_low as u128);
}

pub fn unpcklps(
    cpu: &mut CpuState,
    bus: &mut impl Bus,
    dst: XmmReg,
    src: XmmOperand,
) -> Result<()> {
    let a = u128_to_u32x4(cpu.sse.xmm[dst.index()]);
    let b = u128_to_u32x4(read_xmm_operand_128(cpu, bus, src));
    cpu.sse.xmm[dst.index()] = u32x4_to_u128([a[0], b[0], a[1], b[1]]);
    Ok(())
}

pub fn unpckhps(
    cpu: &mut CpuState,
    bus: &mut impl Bus,
    dst: XmmReg,
    src: XmmOperand,
) -> Result<()> {
    let a = u128_to_u32x4(cpu.sse.xmm[dst.index()]);
    let b = u128_to_u32x4(read_xmm_operand_128(cpu, bus, src));
    cpu.sse.xmm[dst.index()] = u32x4_to_u128([a[2], b[2], a[3], b[3]]);
    Ok(())
}

/// SHUFPS: shuffle packed single-precision floating-point values.
///
/// The low two lanes select from `dst`; the high two lanes select from `src`.
pub fn shufps(
    cpu: &mut CpuState,
    bus: &mut impl Bus,
    dst: XmmReg,
    src: XmmOperand,
    imm8: u8,
) -> Result<()> {
    let a = u128_to_u32x4(cpu.sse.xmm[dst.index()]);
    let b = u128_to_u32x4(read_xmm_operand_128(cpu, bus, src));
    let out = [
        a[(imm8 & 0b11) as usize],
        a[((imm8 >> 2) & 0b11) as usize],
        b[((imm8 >> 4) & 0b11) as usize],
        b[((imm8 >> 6) & 0b11) as usize],
    ];
    cpu.sse.xmm[dst.index()] = u32x4_to_u128(out);
    Ok(())
}

pub fn andps(
    cpu: &mut CpuState,
    bus: &mut impl Bus,
    dst: XmmReg,
    src: XmmOperand,
) -> Result<()> {
    let s = read_xmm_operand_128(cpu, bus, src);
    cpu.sse.xmm[dst.index()] &= s;
    Ok(())
}

pub fn orps(
    cpu: &mut CpuState,
    bus: &mut impl Bus,
    dst: XmmReg,
    src: XmmOperand,
) -> Result<()> {
    let s = read_xmm_operand_128(cpu, bus, src);
    cpu.sse.xmm[dst.index()] |= s;
    Ok(())
}

pub fn xorps(
    cpu: &mut CpuState,
    bus: &mut impl Bus,
    dst: XmmReg,
    src: XmmOperand,
) -> Result<()> {
    let s = read_xmm_operand_128(cpu, bus, src);
    cpu.sse.xmm[dst.index()] ^= s;
    Ok(())
}

pub fn andnps(
    cpu: &mut CpuState,
    bus: &mut impl Bus,
    dst: XmmReg,
    src: XmmOperand,
) -> Result<()> {
    let s = read_xmm_operand_128(cpu, bus, src);
    cpu.sse.xmm[dst.index()] = !cpu.sse.xmm[dst.index()] & s;
    Ok(())
}

pub fn addss(
    cpu: &mut CpuState,
    bus: &mut impl Bus,
    dst: XmmReg,
    src: XmmOperand,
) -> Result<()> {
    scalar_f32_op(cpu, bus, dst, src, |a, b| a + b)
}

pub fn subss(
    cpu: &mut CpuState,
    bus: &mut impl Bus,
    dst: XmmReg,
    src: XmmOperand,
) -> Result<()> {
    scalar_f32_op(cpu, bus, dst, src, |a, b| a - b)
}

pub fn mulss(
    cpu: &mut CpuState,
    bus: &mut impl Bus,
    dst: XmmReg,
    src: XmmOperand,
) -> Result<()> {
    scalar_f32_op(cpu, bus, dst, src, |a, b| a * b)
}

pub fn divss(
    cpu: &mut CpuState,
    bus: &mut impl Bus,
    dst: XmmReg,
    src: XmmOperand,
) -> Result<()> {
    scalar_f32_op(cpu, bus, dst, src, |a, b| a / b)
}

fn scalar_f32_op(
    cpu: &mut CpuState,
    bus: &mut impl Bus,
    dst: XmmReg,
    src: XmmOperand,
    op: impl FnOnce(f32, f32) -> f32,
) -> Result<()> {
    let a_bits = cpu.sse.xmm[dst.index()] as u32;
    let b_bits = read_xmm_operand_u32(cpu, bus, src);
    let a = f32::from_bits(a_bits);
    let b = f32::from_bits(b_bits);
    let res = op(a, b);
    let dst_old = cpu.sse.xmm[dst.index()];
    cpu.sse.xmm[dst.index()] = u128_set_low_u32_preserve(dst_old, res.to_bits());
    Ok(())
}

pub fn addps(
    cpu: &mut CpuState,
    bus: &mut impl Bus,
    dst: XmmReg,
    src: XmmOperand,
) -> Result<()> {
    packed_f32_op(cpu, bus, dst, src, |a, b| a + b)
}

pub fn subps(
    cpu: &mut CpuState,
    bus: &mut impl Bus,
    dst: XmmReg,
    src: XmmOperand,
) -> Result<()> {
    packed_f32_op(cpu, bus, dst, src, |a, b| a - b)
}

pub fn mulps(
    cpu: &mut CpuState,
    bus: &mut impl Bus,
    dst: XmmReg,
    src: XmmOperand,
) -> Result<()> {
    packed_f32_op(cpu, bus, dst, src, |a, b| a * b)
}

pub fn divps(
    cpu: &mut CpuState,
    bus: &mut impl Bus,
    dst: XmmReg,
    src: XmmOperand,
) -> Result<()> {
    packed_f32_op(cpu, bus, dst, src, |a, b| a / b)
}

fn packed_f32_op(
    cpu: &mut CpuState,
    bus: &mut impl Bus,
    dst: XmmReg,
    src: XmmOperand,
    op: impl Fn(f32, f32) -> f32,
) -> Result<()> {
    let a = u128_to_f32x4(cpu.sse.xmm[dst.index()]);
    let b = u128_to_f32x4(read_xmm_operand_128(cpu, bus, src));
    let mut out = [0f32; 4];
    for i in 0..4 {
        out[i] = op(a[i], b[i]);
    }
    cpu.sse.xmm[dst.index()] = f32x4_to_u128(out);
    Ok(())
}

/// CVTSI2SS: dst.low32 = (src as f32), dst.high96 preserved.
pub fn cvtsi2ss(cpu: &mut CpuState, dst: XmmReg, src: i64) {
    let dst_old = cpu.sse.xmm[dst.index()];
    let f = cvt_i64_to_f32(cpu, src);
    cpu.sse.xmm[dst.index()] = u128_set_low_u32_preserve(dst_old, f.to_bits());
}

/// CVTSS2SI (round according to MXCSR.RC).
pub fn cvtss2si32(cpu: &mut CpuState, bus: &mut impl Bus, src: XmmOperand) -> i32 {
    let bits = read_xmm_operand_u32(cpu, bus, src);
    let val = f32::from_bits(bits) as f64;
    cvt_float_to_i32(cpu, val, false)
}

pub fn cvtss2si64(cpu: &mut CpuState, bus: &mut impl Bus, src: XmmOperand) -> i64 {
    let bits = read_xmm_operand_u32(cpu, bus, src);
    let val = f32::from_bits(bits) as f64;
    cvt_float_to_i64(cpu, val, false)
}

/// CVTTSS2SI (truncate regardless of MXCSR.RC).
pub fn cvttss2si32(cpu: &mut CpuState, bus: &mut impl Bus, src: XmmOperand) -> i32 {
    let bits = read_xmm_operand_u32(cpu, bus, src);
    let val = f32::from_bits(bits) as f64;
    cvt_float_to_i32(cpu, val, true)
}

pub fn cvttss2si64(cpu: &mut CpuState, bus: &mut impl Bus, src: XmmOperand) -> i64 {
    let bits = read_xmm_operand_u32(cpu, bus, src);
    let val = f32::from_bits(bits) as f64;
    cvt_float_to_i64(cpu, val, true)
}
