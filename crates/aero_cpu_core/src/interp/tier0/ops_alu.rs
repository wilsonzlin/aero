use super::ops_data::{calc_ea, op_bits, read_op_sized, write_op_sized};
use super::ExecOutcome;
use crate::exception::Exception;
use crate::mem::CpuBus;
use crate::state::{mask_bits, CpuState, FLAG_AF, FLAG_CF, FLAG_OF, FLAG_PF, FLAG_SF, FLAG_ZF};
use aero_x86::{DecodedInst, Instruction, Mnemonic, OpKind, Register};

pub fn handles_mnemonic(m: Mnemonic) -> bool {
    matches!(
        m,
        Mnemonic::Add
            | Mnemonic::Sub
            | Mnemonic::Adc
            | Mnemonic::Sbb
            | Mnemonic::Cmp
            | Mnemonic::Inc
            | Mnemonic::Dec
            | Mnemonic::Neg
            | Mnemonic::Mul
            | Mnemonic::Imul
            | Mnemonic::Div
            | Mnemonic::Idiv
            | Mnemonic::And
            | Mnemonic::Or
            | Mnemonic::Xor
            | Mnemonic::Test
            | Mnemonic::Not
            | Mnemonic::Shl
            | Mnemonic::Shr
            | Mnemonic::Sar
            | Mnemonic::Rol
            | Mnemonic::Ror
            | Mnemonic::Rcl
            | Mnemonic::Rcr
            | Mnemonic::Shld
            | Mnemonic::Shrd
            | Mnemonic::Bt
            | Mnemonic::Bts
            | Mnemonic::Btr
            | Mnemonic::Btc
            | Mnemonic::Bsf
            | Mnemonic::Bsr
            | Mnemonic::Clc
            | Mnemonic::Stc
            | Mnemonic::Cmc
            | Mnemonic::Cld
            | Mnemonic::Std
            | Mnemonic::Cbw
            | Mnemonic::Cwde
            | Mnemonic::Cdqe
            | Mnemonic::Cwd
            | Mnemonic::Cdq
            | Mnemonic::Cqo
    )
}

pub fn exec<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    decoded: &DecodedInst,
    next_ip: u64,
) -> Result<ExecOutcome, Exception> {
    let instr = &decoded.instr;
    if instr.has_lock_prefix() {
        // The LOCK prefix is only valid for a small set of read-modify-write instructions,
        // and only when the destination operand is memory.
        //
        // Intel SDM Vol 2: "LOCK—Lock Asserted Prefix".
        match instr.mnemonic() {
            Mnemonic::Add
            | Mnemonic::Adc
            | Mnemonic::Sub
            | Mnemonic::Sbb
            | Mnemonic::Inc
            | Mnemonic::Dec
            | Mnemonic::Neg
            | Mnemonic::Not
            | Mnemonic::And
            | Mnemonic::Or
            | Mnemonic::Xor
            | Mnemonic::Bts
            | Mnemonic::Btr
            | Mnemonic::Btc => {}
            _ => return Err(Exception::InvalidOpcode),
        }
    }
    match instr.mnemonic() {
        Mnemonic::Add | Mnemonic::Adc | Mnemonic::Sub | Mnemonic::Sbb | Mnemonic::Cmp => {
            let bits = op_bits(state, instr, 0)?;
            let src = read_op_sized(state, bus, instr, 1, bits, next_ip)?;
            let carry_in = match instr.mnemonic() {
                Mnemonic::Adc | Mnemonic::Sbb => state.get_flag(FLAG_CF) as u64,
                _ => 0,
            };

            if instr.has_lock_prefix() {
                // LOCK is only valid for RMW operations with a memory destination operand.
                if instr.mnemonic() == Mnemonic::Cmp || instr.op_kind(0) != OpKind::Memory {
                    return Err(Exception::InvalidOpcode);
                }

                let addr = calc_ea(state, instr, next_ip, true)?;
                let old = super::atomic_rmw_sized(bus, addr, bits, |old| {
                    let rhs = src.wrapping_add(carry_in);
                    let res = match instr.mnemonic() {
                        Mnemonic::Add | Mnemonic::Adc => old.wrapping_add(rhs),
                        Mnemonic::Sub | Mnemonic::Sbb => old.wrapping_sub(rhs),
                        _ => old,
                    } & mask_bits(bits);
                    (res, old)
                })?;
                let (_, flags) = match instr.mnemonic() {
                    Mnemonic::Add | Mnemonic::Adc => {
                        add_with_flags(state, old, src, carry_in, bits)
                    }
                    Mnemonic::Sub | Mnemonic::Sbb => {
                        sub_with_flags(state, old, src, carry_in, bits)
                    }
                    _ => unreachable!(),
                };
                state.set_rflags(flags);
                return Ok(ExecOutcome::Continue);
            }

            let dst = read_op_sized(state, bus, instr, 0, bits, next_ip)?;
            let (res, flags) = match instr.mnemonic() {
                Mnemonic::Add | Mnemonic::Adc => add_with_flags(state, dst, src, carry_in, bits),
                Mnemonic::Sub | Mnemonic::Sbb | Mnemonic::Cmp => {
                    sub_with_flags(state, dst, src, carry_in, bits)
                }
                _ => unreachable!(),
            };
            state.set_rflags(flags);
            if instr.mnemonic() != Mnemonic::Cmp {
                write_op_sized(state, bus, instr, 0, res, bits, next_ip)?;
            }
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Inc | Mnemonic::Dec => {
            let bits = op_bits(state, instr, 0)?;
            let cf = state.get_flag(FLAG_CF);
            if instr.has_lock_prefix() {
                if instr.op_kind(0) != OpKind::Memory {
                    return Err(Exception::InvalidOpcode);
                }
                let addr = calc_ea(state, instr, next_ip, true)?;
                let old = super::atomic_rmw_sized(bus, addr, bits, |old| {
                    let mask = mask_bits(bits);
                    let new = match instr.mnemonic() {
                        Mnemonic::Inc => old.wrapping_add(1) & mask,
                        Mnemonic::Dec => old.wrapping_sub(1) & mask,
                        _ => old,
                    };
                    (new, old)
                })?;
                let (_res, flags) = if instr.mnemonic() == Mnemonic::Inc {
                    add_with_flags(state, old, 1, 0, bits)
                } else {
                    sub_with_flags(state, old, 1, 0, bits)
                };
                // INC/DEC don't modify CF.
                state.set_rflags((flags & !FLAG_CF) | (cf as u64 * FLAG_CF));
                Ok(ExecOutcome::Continue)
            } else {
                let dst = read_op_sized(state, bus, instr, 0, bits, next_ip)?;
                let (res, flags) = if instr.mnemonic() == Mnemonic::Inc {
                    add_with_flags(state, dst, 1, 0, bits)
                } else {
                    sub_with_flags(state, dst, 1, 0, bits)
                };
                // INC/DEC don't modify CF.
                state.set_rflags((flags & !FLAG_CF) | (cf as u64 * FLAG_CF));
                write_op_sized(state, bus, instr, 0, res, bits, next_ip)?;
                Ok(ExecOutcome::Continue)
            }
        }
        Mnemonic::Neg => {
            let bits = op_bits(state, instr, 0)?;
            if instr.has_lock_prefix() {
                if instr.op_kind(0) != OpKind::Memory {
                    return Err(Exception::InvalidOpcode);
                }
                let addr = calc_ea(state, instr, next_ip, true)?;
                let old = super::atomic_rmw_sized(bus, addr, bits, |old| {
                    let res = (!old).wrapping_add(1) & mask_bits(bits);
                    (res, old)
                })?;
                let (_, flags) = neg_with_flags(state, old, bits);
                state.set_rflags(flags);
                return Ok(ExecOutcome::Continue);
            }
            let dst = read_op_sized(state, bus, instr, 0, bits, next_ip)?;
            let (res, flags) = neg_with_flags(state, dst, bits);
            state.set_rflags(flags);
            write_op_sized(state, bus, instr, 0, res, bits, next_ip)?;
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::And | Mnemonic::Or | Mnemonic::Xor | Mnemonic::Test => {
            let bits = op_bits(state, instr, 0)?;
            let src = read_op_sized(state, bus, instr, 1, bits, next_ip)?;

            if instr.has_lock_prefix() {
                if instr.mnemonic() == Mnemonic::Test || instr.op_kind(0) != OpKind::Memory {
                    return Err(Exception::InvalidOpcode);
                }
                let addr = calc_ea(state, instr, next_ip, true)?;
                let old = super::atomic_rmw_sized(bus, addr, bits, |old| {
                    let res = match instr.mnemonic() {
                        Mnemonic::And => old & src,
                        Mnemonic::Or => old | src,
                        Mnemonic::Xor => old ^ src,
                        _ => old,
                    } & mask_bits(bits);
                    (res, old)
                })?;
                let res = match instr.mnemonic() {
                    Mnemonic::And => old & src,
                    Mnemonic::Or => old | src,
                    Mnemonic::Xor => old ^ src,
                    _ => old,
                } & mask_bits(bits);
                let flags = logic_flags(state, res, bits);
                state.set_rflags(flags);
                return Ok(ExecOutcome::Continue);
            }

            let dst = read_op_sized(state, bus, instr, 0, bits, next_ip)?;
            let res = match instr.mnemonic() {
                Mnemonic::And | Mnemonic::Test => dst & src,
                Mnemonic::Or => dst | src,
                Mnemonic::Xor => dst ^ src,
                _ => unreachable!(),
            } & mask_bits(bits);
            let flags = logic_flags(state, res, bits);
            state.set_rflags(flags);
            if instr.mnemonic() != Mnemonic::Test {
                write_op_sized(state, bus, instr, 0, res, bits, next_ip)?;
            }
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Not => {
            let bits = op_bits(state, instr, 0)?;
            if instr.has_lock_prefix() {
                if instr.op_kind(0) != OpKind::Memory {
                    return Err(Exception::InvalidOpcode);
                }
                let addr = calc_ea(state, instr, next_ip, true)?;
                super::atomic_rmw_sized(bus, addr, bits, |old| {
                    let res = (!old) & mask_bits(bits);
                    (res, ())
                })?;
                return Ok(ExecOutcome::Continue);
            }
            let dst = read_op_sized(state, bus, instr, 0, bits, next_ip)?;
            let res = (!dst) & mask_bits(bits);
            write_op_sized(state, bus, instr, 0, res, bits, next_ip)?;
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Mul | Mnemonic::Imul => exec_mul(state, bus, instr, next_ip),
        Mnemonic::Div | Mnemonic::Idiv => exec_div(state, bus, instr, next_ip),
        Mnemonic::Shl
        | Mnemonic::Shr
        | Mnemonic::Sar
        | Mnemonic::Rol
        | Mnemonic::Ror
        | Mnemonic::Rcl
        | Mnemonic::Rcr => exec_shift_rotate(state, bus, instr, next_ip),
        Mnemonic::Shld | Mnemonic::Shrd => exec_shift_double(state, bus, instr, next_ip),
        Mnemonic::Bt | Mnemonic::Bts | Mnemonic::Btr | Mnemonic::Btc => {
            exec_bit_test(state, bus, instr, next_ip)
        }
        Mnemonic::Bsf | Mnemonic::Bsr => exec_bsf_bsr(state, bus, instr, next_ip),
        Mnemonic::Clc => {
            state.set_flag(FLAG_CF, false);
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Stc => {
            state.set_flag(FLAG_CF, true);
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Cmc => {
            let cf = state.get_flag(FLAG_CF);
            state.set_flag(FLAG_CF, !cf);
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Cld => {
            state.set_flag(crate::state::FLAG_DF, false);
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Std => {
            state.set_flag(crate::state::FLAG_DF, true);
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Cbw | Mnemonic::Cwde | Mnemonic::Cdqe => {
            exec_cbx(state, instr.mnemonic());
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Cwd | Mnemonic::Cdq | Mnemonic::Cqo => {
            exec_cdx(state, instr.mnemonic());
            Ok(ExecOutcome::Continue)
        }
        _ => Err(Exception::InvalidOpcode),
    }
}

pub(crate) fn add_with_flags(
    state: &CpuState,
    a: u64,
    b: u64,
    carry_in: u64,
    bits: u32,
) -> (u64, u64) {
    let mask = mask_bits(bits);
    let a_m = a & mask;
    let b_m = b & mask;
    let full = (a_m as u128) + (b_m as u128) + (carry_in as u128);
    let res = (full as u64) & mask;

    let cf = full > (mask as u128);
    let sign_mask = 1u64 << (bits - 1);
    let of = ((a_m ^ res) & (b_m ^ res) & sign_mask) != 0;
    let af = ((a_m ^ b_m ^ res) & 0x10) != 0;

    let mut flags = state.rflags() & !(FLAG_CF | FLAG_OF | FLAG_SF | FLAG_ZF | FLAG_PF | FLAG_AF);
    set_logic_szp(&mut flags, res, bits);
    if cf {
        flags |= FLAG_CF;
    }
    if of {
        flags |= FLAG_OF;
    }
    if af {
        flags |= FLAG_AF;
    }
    (res, flags)
}

pub(crate) fn sub_with_flags(
    state: &CpuState,
    a: u64,
    b: u64,
    borrow_in: u64,
    bits: u32,
) -> (u64, u64) {
    let mask = mask_bits(bits);
    let a_m = a & mask;
    let b_m = b & mask;
    let subtrahend = (b_m as u128) + (borrow_in as u128);
    let minuend = a_m as u128;
    let full = (minuend + (1u128 << bits)) - subtrahend;
    let res = (full as u64) & mask;

    let cf = minuend < subtrahend;
    let sign_mask = 1u64 << (bits - 1);
    let of = ((a_m ^ b_m) & (a_m ^ res) & sign_mask) != 0;
    let af = ((a_m ^ b_m ^ res) & 0x10) != 0;

    let mut flags = state.rflags() & !(FLAG_CF | FLAG_OF | FLAG_SF | FLAG_ZF | FLAG_PF | FLAG_AF);
    set_logic_szp(&mut flags, res, bits);
    if cf {
        flags |= FLAG_CF;
    }
    if of {
        flags |= FLAG_OF;
    }
    if af {
        flags |= FLAG_AF;
    }
    (res, flags)
}

fn neg_with_flags(state: &CpuState, v: u64, bits: u32) -> (u64, u64) {
    let mask = mask_bits(bits);
    let v_m = v & mask;
    let res = (!v_m).wrapping_add(1) & mask;

    let cf = v_m != 0;
    let of = v_m == (1u64 << (bits - 1));
    let af = (v_m & 0xF) != 0;

    let mut flags = state.rflags() & !(FLAG_CF | FLAG_OF | FLAG_SF | FLAG_ZF | FLAG_PF | FLAG_AF);
    set_logic_szp(&mut flags, res, bits);
    if cf {
        flags |= FLAG_CF;
    }
    if of {
        flags |= FLAG_OF;
    }
    if af {
        flags |= FLAG_AF;
    }
    (res, flags)
}

fn logic_flags(state: &CpuState, res: u64, bits: u32) -> u64 {
    let mut flags = state.rflags() & !(FLAG_CF | FLAG_OF | FLAG_SF | FLAG_ZF | FLAG_PF | FLAG_AF);
    set_logic_szp(&mut flags, res, bits);
    // AND/OR/XOR/TEST clear CF and OF; AF undefined (clear for determinism).
    flags
}

fn set_logic_szp(flags: &mut u64, res: u64, bits: u32) {
    let r = res & mask_bits(bits);
    if r == 0 {
        *flags |= FLAG_ZF;
    }
    if (r & (1u64 << (bits - 1))) != 0 {
        *flags |= FLAG_SF;
    }
    if parity8(r as u8) {
        *flags |= FLAG_PF;
    }
}

fn parity8(v: u8) -> bool {
    v.count_ones() % 2 == 0
}

fn exec_mul<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<ExecOutcome, Exception> {
    let bits = op_bits(state, instr, 0)?;
    let src = read_op_sized(state, bus, instr, 0, bits, next_ip)?;

    match instr.mnemonic() {
        Mnemonic::Mul => {
            let overflow = match bits {
                8 => {
                    let al = state.read_reg(Register::AL) as u8;
                    let m = src as u8;
                    let full = (al as u16) * (m as u16);
                    state.write_reg(Register::AX, full as u64);
                    (full >> 8) != 0
                }
                16 => {
                    let ax = state.read_reg(Register::AX) as u16;
                    let m = src as u16;
                    let full = (ax as u32) * (m as u32);
                    state.write_reg(Register::AX, (full & 0xFFFF) as u64);
                    state.write_reg(Register::DX, (full >> 16) as u64);
                    (full >> 16) != 0
                }
                32 => {
                    let eax = state.read_reg(Register::EAX) as u32;
                    let m = src as u32;
                    let full = (eax as u64) * (m as u64);
                    state.write_reg(Register::EAX, (full as u32) as u64);
                    state.write_reg(Register::EDX, ((full >> 32) as u32) as u64);
                    (full >> 32) != 0
                }
                64 => {
                    let rax = state.read_reg(Register::RAX);
                    let full = (rax as u128) * (src as u128);
                    state.write_reg(Register::RAX, full as u64);
                    state.write_reg(Register::RDX, (full >> 64) as u64);
                    (full >> 64) != 0
                }
                _ => return Err(Exception::InvalidOpcode),
            };
            state.set_flag(FLAG_CF, overflow);
            state.set_flag(FLAG_OF, overflow);
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Imul => {
            if instr.op_count() == 1 {
                let overflow = match bits {
                    8 => {
                        let al = state.read_reg(Register::AL) as u8 as i8 as i16;
                        let m = src as u8 as i8 as i16;
                        let full = al * m; // i16
                        state.write_reg(Register::AX, (full as u16) as u64);
                        full != (full as i8 as i16)
                    }
                    16 => {
                        let ax = state.read_reg(Register::AX) as u16 as i16 as i32;
                        let m = src as u16 as i16 as i32;
                        let full = ax * m; // i32
                        state.write_reg(Register::AX, (full as u16) as u64);
                        state.write_reg(Register::DX, ((full >> 16) as u16) as u64);
                        full != (full as i16 as i32)
                    }
                    32 => {
                        let eax = state.read_reg(Register::EAX) as u32 as i32 as i64;
                        let m = src as u32 as i32 as i64;
                        let full = eax * m; // i64
                        state.write_reg(Register::EAX, (full as u32) as u64);
                        state.write_reg(Register::EDX, ((full >> 32) as u32) as u64);
                        full != (full as i32 as i64)
                    }
                    64 => {
                        let rax = state.read_reg(Register::RAX) as i64 as i128;
                        let m = src as i64 as i128;
                        let full = rax * m; // i128
                        state.write_reg(Register::RAX, full as u64);
                        state.write_reg(Register::RDX, (full >> 64) as u64);
                        full != (full as i64 as i128)
                    }
                    _ => return Err(Exception::InvalidOpcode),
                };
                state.set_flag(FLAG_CF, overflow);
                state.set_flag(FLAG_OF, overflow);
                Ok(ExecOutcome::Continue)
            } else {
                // 2-operand / 3-operand forms
                let dst_bits = op_bits(state, instr, 0)?;
                let (a, b) = if instr.op_count() == 3 {
                    (
                        read_op_sized(state, bus, instr, 1, dst_bits, next_ip)?,
                        read_op_sized(state, bus, instr, 2, dst_bits, next_ip)?,
                    )
                } else {
                    (
                        read_op_sized(state, bus, instr, 0, dst_bits, next_ip)?,
                        read_op_sized(state, bus, instr, 1, dst_bits, next_ip)?,
                    )
                };
                let full = sign_ext_to_i128(a, dst_bits) * sign_ext_to_i128(b, dst_bits);
                let res = (full as u128 & (mask_bits(dst_bits) as u128)) as u64;
                write_op_sized(state, bus, instr, 0, res, dst_bits, next_ip)?;
                let overflow = full != sign_ext_to_i128(res, dst_bits);
                state.set_flag(FLAG_CF, overflow);
                state.set_flag(FLAG_OF, overflow);
                Ok(ExecOutcome::Continue)
            }
        }
        _ => Err(Exception::InvalidOpcode),
    }
}

fn exec_div<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<ExecOutcome, Exception> {
    let bits = op_bits(state, instr, 0)?;
    let divisor = read_op_sized(state, bus, instr, 0, bits, next_ip)?;
    if divisor == 0 {
        return Err(Exception::DivideError);
    }
    match instr.mnemonic() {
        Mnemonic::Div => {
            match bits {
                8 => {
                    let dividend = state.read_reg(Register::AX) as u16;
                    let div = divisor as u8 as u16;
                    let q = dividend / div;
                    let r = dividend % div;
                    if q > 0xFF {
                        return Err(Exception::DivideError);
                    }
                    state.write_reg(Register::AL, q as u64);
                    state.write_reg(Register::AH, r as u64);
                }
                16 => {
                    let ax = state.read_reg(Register::AX) as u16;
                    let dx = state.read_reg(Register::DX) as u16;
                    let dividend = ((dx as u32) << 16) | (ax as u32);
                    let div = divisor as u16 as u32;
                    let q = dividend / div;
                    let r = dividend % div;
                    if q > 0xFFFF {
                        return Err(Exception::DivideError);
                    }
                    state.write_reg(Register::AX, q as u64);
                    state.write_reg(Register::DX, r as u64);
                }
                32 => {
                    let eax = state.read_reg(Register::EAX) as u32;
                    let edx = state.read_reg(Register::EDX) as u32;
                    let dividend = ((edx as u64) << 32) | (eax as u64);
                    let div = divisor as u32 as u64;
                    let q = dividend / div;
                    let r = dividend % div;
                    if q > 0xFFFF_FFFF {
                        return Err(Exception::DivideError);
                    }
                    state.write_reg(Register::EAX, q as u64);
                    state.write_reg(Register::EDX, r as u64);
                }
                64 => {
                    let rax = state.read_reg(Register::RAX);
                    let rdx = state.read_reg(Register::RDX);
                    let dividend = ((rdx as u128) << 64) | (rax as u128);
                    let div = divisor as u128;
                    let q = dividend / div;
                    let r = dividend % div;
                    if q > u64::MAX as u128 {
                        return Err(Exception::DivideError);
                    }
                    state.write_reg(Register::RAX, q as u64);
                    state.write_reg(Register::RDX, r as u64);
                }
                _ => return Err(Exception::InvalidOpcode),
            }
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Idiv => {
            match bits {
                8 => {
                    let dividend = state.read_reg(Register::AX) as u16 as i16;
                    let div = divisor as u8 as i8 as i16;
                    let q = dividend / div;
                    let r = dividend % div;
                    if q < i8::MIN as i16 || q > i8::MAX as i16 {
                        return Err(Exception::DivideError);
                    }
                    state.write_reg(Register::AL, (q as i8 as u8) as u64);
                    state.write_reg(Register::AH, (r as i8 as u8) as u64);
                }
                16 => {
                    let ax = state.read_reg(Register::AX) as u16 as i16 as i32;
                    let dx = state.read_reg(Register::DX) as u16 as i16 as i32;
                    let dividend = (dx << 16) | (ax & 0xFFFF);
                    let div = divisor as u16 as i16 as i32;
                    let q = dividend / div;
                    let r = dividend % div;
                    if q < i16::MIN as i32 || q > i16::MAX as i32 {
                        return Err(Exception::DivideError);
                    }
                    state.write_reg(Register::AX, (q as i16 as u16) as u64);
                    state.write_reg(Register::DX, (r as i16 as u16) as u64);
                }
                32 => {
                    let eax = state.read_reg(Register::EAX) as u32 as i32 as i64;
                    let edx = state.read_reg(Register::EDX) as u32 as i32 as i64;
                    let dividend = (edx << 32) | (eax & 0xFFFF_FFFF);
                    let div = divisor as u32 as i32 as i64;
                    let q = dividend / div;
                    let r = dividend % div;
                    if q < i32::MIN as i64 || q > i32::MAX as i64 {
                        return Err(Exception::DivideError);
                    }
                    state.write_reg(Register::EAX, (q as i32 as u32) as u64);
                    state.write_reg(Register::EDX, (r as i32 as u32) as u64);
                }
                64 => {
                    let rax = state.read_reg(Register::RAX) as i64 as i128;
                    let rdx = state.read_reg(Register::RDX) as i64 as i128;
                    let dividend = (rdx << 64) | (rax & 0xFFFF_FFFF_FFFF_FFFF);
                    let div = divisor as i64 as i128;
                    let q = dividend / div;
                    let r = dividend % div;
                    if q < i64::MIN as i128 || q > i64::MAX as i128 {
                        return Err(Exception::DivideError);
                    }
                    state.write_reg(Register::RAX, q as u64);
                    state.write_reg(Register::RDX, r as u64);
                }
                _ => return Err(Exception::InvalidOpcode),
            }
            Ok(ExecOutcome::Continue)
        }
        _ => Err(Exception::InvalidOpcode),
    }
}

fn sign_ext_to_i128(v: u64, bits: u32) -> i128 {
    let v_m = v & mask_bits(bits);
    if bits == 64 {
        (v_m as i64) as i128
    } else {
        let sign_bit = 1u64 << (bits - 1);
        let extended = if (v_m & sign_bit) != 0 {
            v_m | (!0u64 << bits)
        } else {
            v_m
        };
        (extended as i64) as i128
    }
}

fn exec_shift_rotate<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<ExecOutcome, Exception> {
    let bits = op_bits(state, instr, 0)?;
    let dst = read_op_sized(state, bus, instr, 0, bits, next_ip)?;
    let count = read_shift_count(state, bus, instr, next_ip)? as u32;
    let (res, flags_opt) = match instr.mnemonic() {
        Mnemonic::Shl => shl(dst, count, bits, state.rflags()),
        Mnemonic::Shr => shr(dst, count, bits, state.rflags()),
        Mnemonic::Sar => sar(dst, count, bits, state.rflags()),
        Mnemonic::Rol => rol(dst, count, bits, state.rflags()),
        Mnemonic::Ror => ror(dst, count, bits, state.rflags()),
        Mnemonic::Rcl => rcl(dst, count, bits, state.rflags()),
        Mnemonic::Rcr => rcr(dst, count, bits, state.rflags()),
        _ => return Err(Exception::InvalidOpcode),
    };
    if let Some(f) = flags_opt {
        state.set_rflags(f);
    }
    if count != 0 {
        write_op_sized(state, bus, instr, 0, res, bits, next_ip)?;
    }
    Ok(ExecOutcome::Continue)
}

fn read_shift_count<B: CpuBus>(
    state: &mut CpuState,
    _bus: &mut B,
    instr: &Instruction,
    _next_ip: u64,
) -> Result<u64, Exception> {
    let count_op = match instr.mnemonic() {
        Mnemonic::Shld | Mnemonic::Shrd => 2,
        _ => 1,
    };
    match instr.op_kind(count_op) {
        OpKind::Immediate8 => Ok(instr.immediate8() as u64),
        OpKind::Register => {
            let reg = instr.op_register(count_op);
            Ok(state.read_reg(reg) & 0xFF)
        }
        _ => Err(Exception::InvalidOpcode),
    }
}

fn shl(val: u64, count: u32, bits: u32, old_flags: u64) -> (u64, Option<u64>) {
    let mask = mask_bits(bits);
    let c = count & if bits == 64 { 0x3F } else { 0x1F };
    if c == 0 {
        return (val & mask, None);
    }
    let res = (val << c) & mask;
    let cf = ((val >> (bits - c)) & 1) != 0;
    let mut flags = old_flags & !(FLAG_CF | FLAG_OF | FLAG_SF | FLAG_ZF | FLAG_PF | FLAG_AF);
    set_logic_szp(&mut flags, res, bits);
    if cf {
        flags |= FLAG_CF;
    }
    if c == 1 {
        let of = ((res >> (bits - 1)) & 1) ^ (cf as u64);
        if of != 0 {
            flags |= FLAG_OF;
        }
    }
    (res, Some(flags))
}

fn shr(val: u64, count: u32, bits: u32, old_flags: u64) -> (u64, Option<u64>) {
    let mask = mask_bits(bits);
    let c = count & if bits == 64 { 0x3F } else { 0x1F };
    if c == 0 {
        return (val & mask, None);
    }
    let res = (val & mask) >> c;
    let cf = ((val >> (c - 1)) & 1) != 0;
    let mut flags = old_flags & !(FLAG_CF | FLAG_OF | FLAG_SF | FLAG_ZF | FLAG_PF | FLAG_AF);
    set_logic_szp(&mut flags, res, bits);
    if cf {
        flags |= FLAG_CF;
    }
    if c == 1 {
        let of = ((val >> (bits - 1)) & 1) != 0;
        if of {
            flags |= FLAG_OF;
        }
    }
    (res, Some(flags))
}

fn sar(val: u64, count: u32, bits: u32, old_flags: u64) -> (u64, Option<u64>) {
    let mask = mask_bits(bits);
    let c = count & if bits == 64 { 0x3F } else { 0x1F };
    if c == 0 {
        return (val & mask, None);
    }
    let signed = sign_ext_to_i128(val, bits);
    let res = ((signed >> c) as u64) & mask;
    let cf = ((val >> (c - 1)) & 1) != 0;
    let mut flags = old_flags & !(FLAG_CF | FLAG_OF | FLAG_SF | FLAG_ZF | FLAG_PF | FLAG_AF);
    set_logic_szp(&mut flags, res, bits);
    if cf {
        flags |= FLAG_CF;
    }
    if c == 1 {
        // OF cleared for SAR by 1
    }
    (res, Some(flags))
}

fn rol(val: u64, count: u32, bits: u32, old_flags: u64) -> (u64, Option<u64>) {
    let mask = mask_bits(bits);
    let mut c = count & if bits == 64 { 0x3F } else { 0x1F };
    if c == 0 {
        return (val & mask, None);
    }
    c %= bits;
    let res = ((val << c) | (val >> (bits - c))) & mask;
    let cf = (res & 1) != 0;
    let mut flags = old_flags & !(FLAG_CF | FLAG_OF);
    if cf {
        flags |= FLAG_CF;
    }
    if c == 1 {
        let of = ((res >> (bits - 1)) & 1) ^ (cf as u64);
        if of != 0 {
            flags |= FLAG_OF;
        }
    }
    (res, Some(flags))
}

fn ror(val: u64, count: u32, bits: u32, old_flags: u64) -> (u64, Option<u64>) {
    let mask = mask_bits(bits);
    let mut c = count & if bits == 64 { 0x3F } else { 0x1F };
    if c == 0 {
        return (val & mask, None);
    }
    c %= bits;
    let res = ((val >> c) | (val << (bits - c))) & mask;
    let cf = ((res >> (bits - 1)) & 1) != 0;
    let mut flags = old_flags & !(FLAG_CF | FLAG_OF);
    if cf {
        flags |= FLAG_CF;
    }
    if c == 1 {
        let msb = (res >> (bits - 1)) & 1;
        let msb2 = (res >> (bits - 2)) & 1;
        if (msb ^ msb2) != 0 {
            flags |= FLAG_OF;
        }
    }
    (res, Some(flags))
}

fn rcl(val: u64, count: u32, bits: u32, old_flags: u64) -> (u64, Option<u64>) {
    let cf_in = (old_flags & FLAG_CF) != 0;
    let width = bits + 1;
    let mut c = count & if bits == 64 { 0x3F } else { 0x1F };
    if c == 0 {
        return (val & mask_bits(bits), None);
    }
    c %= width;
    let mask_ext = (1u128 << width) - 1;
    let mut ext = ((val & mask_bits(bits)) as u128) | ((cf_in as u128) << bits);
    ext = ((ext << c) | (ext >> (width - c))) & mask_ext;
    let res = (ext as u64) & mask_bits(bits);
    let cf = ((ext >> bits) & 1) != 0;
    let mut flags = old_flags & !(FLAG_CF | FLAG_OF);
    if cf {
        flags |= FLAG_CF;
    }
    if c == 1 {
        let of = ((res >> (bits - 1)) & 1) ^ (cf as u64);
        if of != 0 {
            flags |= FLAG_OF;
        }
    }
    (res, Some(flags))
}

fn rcr(val: u64, count: u32, bits: u32, old_flags: u64) -> (u64, Option<u64>) {
    let cf_in = (old_flags & FLAG_CF) != 0;
    let width = bits + 1;
    let mut c = count & if bits == 64 { 0x3F } else { 0x1F };
    if c == 0 {
        return (val & mask_bits(bits), None);
    }
    c %= width;
    let mask_ext = (1u128 << width) - 1;
    let mut ext = ((val & mask_bits(bits)) as u128) | ((cf_in as u128) << bits);
    ext = ((ext >> c) | (ext << (width - c))) & mask_ext;
    let res = (ext as u64) & mask_bits(bits);
    let cf = ((ext >> bits) & 1) != 0;
    let mut flags = old_flags & !(FLAG_CF | FLAG_OF);
    if cf {
        flags |= FLAG_CF;
    }
    if c == 1 {
        let msb = (res >> (bits - 1)) & 1;
        let msb2 = (res >> (bits - 2)) & 1;
        if (msb ^ msb2) != 0 {
            flags |= FLAG_OF;
        }
    }
    (res, Some(flags))
}

fn exec_shift_double<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<ExecOutcome, Exception> {
    let bits = op_bits(state, instr, 0)?;
    let dst = read_op_sized(state, bus, instr, 0, bits, next_ip)?;
    let src = read_op_sized(state, bus, instr, 1, bits, next_ip)?;
    let count = read_shift_count(state, bus, instr, next_ip)? as u32;
    let c = count & if bits == 64 { 0x3F } else { 0x1F };
    if c == 0 {
        return Ok(ExecOutcome::Continue);
    }
    if c > bits {
        // Undefined architecturally; keep deterministic.
        return Ok(ExecOutcome::Continue);
    }
    let mask = mask_bits(bits);
    let (res, cf) = match instr.mnemonic() {
        Mnemonic::Shld => {
            let res = ((dst << c) | (src >> (bits - c))) & mask;
            let cf = ((dst >> (bits - c)) & 1) != 0;
            (res, cf)
        }
        Mnemonic::Shrd => {
            let res = ((dst >> c) | (src << (bits - c))) & mask;
            let cf = ((dst >> (c - 1)) & 1) != 0;
            (res, cf)
        }
        _ => return Err(Exception::InvalidOpcode),
    };
    let mut flags = state.rflags() & !(FLAG_CF | FLAG_OF | FLAG_SF | FLAG_ZF | FLAG_PF | FLAG_AF);
    set_logic_szp(&mut flags, res, bits);
    if cf {
        flags |= FLAG_CF;
    }
    if c == 1 {
        let of = ((res >> (bits - 1)) & 1) ^ ((dst >> (bits - 1)) & 1);
        if of != 0 {
            flags |= FLAG_OF;
        }
    }
    state.set_rflags(flags);
    write_op_sized(state, bus, instr, 0, res, bits, next_ip)?;
    Ok(ExecOutcome::Continue)
}

fn exec_bit_test<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<ExecOutcome, Exception> {
    let bits = op_bits(state, instr, 0)?;
    let index_bits = op_bits(state, instr, 1)?;
    let bit_index_raw = read_op_sized(state, bus, instr, 1, index_bits, next_ip)?;
    let bit_index = sign_extend_to_i64(bit_index_raw, index_bits);

    match instr.op_kind(0) {
        OpKind::Register => {
            if instr.has_lock_prefix() {
                return Err(Exception::InvalidOpcode);
            }
            let reg = instr.op0_register();
            let val = state.read_reg(reg) & mask_bits(bits);
            let bit = (bit_index as u64) & (bits as u64 - 1);
            let old = ((val >> bit) & 1) != 0;
            state.set_flag(FLAG_CF, old);
            let new_val = match instr.mnemonic() {
                Mnemonic::Bt => val,
                Mnemonic::Bts => val | (1u64 << bit),
                Mnemonic::Btr => val & !(1u64 << bit),
                Mnemonic::Btc => val ^ (1u64 << bit),
                _ => val,
            };
            if instr.mnemonic() != Mnemonic::Bt {
                state.write_reg(reg, new_val);
            }
            Ok(ExecOutcome::Continue)
        }
        OpKind::Memory => {
            let base_addr = calc_ea(state, instr, next_ip, true)?;
            let (addr, bit) = bit_mem_addr(base_addr, bit_index, bits);

            if instr.has_lock_prefix() {
                // Intel SDM: `LOCK` is only valid for the read-modify-write variants.
                match instr.mnemonic() {
                    Mnemonic::Bts | Mnemonic::Btr | Mnemonic::Btc => {}
                    _ => return Err(Exception::InvalidOpcode),
                }
                let old_bit = super::atomic_rmw_sized(bus, addr, bits, |val| {
                    let old = ((val >> bit) & 1) != 0;
                    let res = match instr.mnemonic() {
                        Mnemonic::Bts => val | (1u64 << bit),
                        Mnemonic::Btr => val & !(1u64 << bit),
                        Mnemonic::Btc => val ^ (1u64 << bit),
                        _ => val,
                    };
                    (res, old)
                })?;
                state.set_flag(FLAG_CF, old_bit);
                Ok(ExecOutcome::Continue)
            } else {
                let val = super::ops_data::read_mem(bus, addr, bits)?;
                let old = ((val >> bit) & 1) != 0;
                state.set_flag(FLAG_CF, old);
                let new_val = match instr.mnemonic() {
                    Mnemonic::Bt => val,
                    Mnemonic::Bts => val | (1u64 << bit),
                    Mnemonic::Btr => val & !(1u64 << bit),
                    Mnemonic::Btc => val ^ (1u64 << bit),
                    _ => val,
                };
                if instr.mnemonic() != Mnemonic::Bt {
                    super::ops_data::write_mem(bus, addr, bits, new_val)?;
                }
                Ok(ExecOutcome::Continue)
            }
        }
        _ => Err(Exception::InvalidOpcode),
    }
}

fn bit_mem_addr(base: u64, bit_index: i64, bits: u32) -> (u64, u32) {
    let unit_bits = bits as i64;
    let unit_bytes = (bits / 8) as i64;
    let word_off = bit_index.div_euclid(unit_bits);
    let bit = bit_index.rem_euclid(unit_bits) as u32;
    let addr = base.wrapping_add((word_off * unit_bytes) as u64);
    (addr, bit)
}

fn sign_extend_to_i64(v: u64, bits: u32) -> i64 {
    let masked = v & mask_bits(bits);
    if bits == 64 {
        masked as i64
    } else {
        let sign_bit = 1u64 << (bits - 1);
        let extended = if (masked & sign_bit) != 0 {
            masked | (!0u64 << bits)
        } else {
            masked
        };
        extended as i64
    }
}

fn exec_bsf_bsr<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<ExecOutcome, Exception> {
    let bits = op_bits(state, instr, 1)?;
    let src = read_op_sized(state, bus, instr, 1, bits, next_ip)?;
    if src == 0 {
        state.set_flag(FLAG_ZF, true);
        return Ok(ExecOutcome::Continue);
    }
    state.set_flag(FLAG_ZF, false);
    let idx = if instr.mnemonic() == Mnemonic::Bsf {
        src.trailing_zeros()
    } else {
        (63 - src.leading_zeros()) as u32
    } as u64;
    write_op_sized(
        state,
        bus,
        instr,
        0,
        idx,
        op_bits(state, instr, 0)?,
        next_ip,
    )?;
    Ok(ExecOutcome::Continue)
}

fn exec_cbx(state: &mut CpuState, m: Mnemonic) {
    match m {
        Mnemonic::Cbw => {
            let al = state.read_reg(Register::AL) as i8 as i16 as u16;
            state.write_reg(Register::AX, al as u64);
        }
        Mnemonic::Cwde => {
            let ax = state.read_reg(Register::AX) as i16 as i32 as u32;
            state.write_reg(Register::EAX, ax as u64);
        }
        Mnemonic::Cdqe => {
            let eax = state.read_reg(Register::EAX) as i32 as i64;
            state.write_reg(Register::RAX, eax as u64);
        }
        _ => {}
    }
}

fn exec_cdx(state: &mut CpuState, m: Mnemonic) {
    match m {
        Mnemonic::Cwd => {
            let ax = state.read_reg(Register::AX) as i16;
            state.write_reg(Register::DX, if ax < 0 { 0xFFFF } else { 0 });
        }
        Mnemonic::Cdq => {
            let eax = state.read_reg(Register::EAX) as i32;
            state.write_reg(Register::EDX, if eax < 0 { 0xFFFF_FFFF } else { 0 });
        }
        Mnemonic::Cqo => {
            let rax = state.read_reg(Register::RAX) as i64;
            state.write_reg(Register::RDX, if rax < 0 { u64::MAX } else { 0 });
        }
        _ => {}
    }
}
