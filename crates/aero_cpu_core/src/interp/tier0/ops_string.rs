use super::ExecOutcome;
use crate::exception::Exception;
use crate::mem::CpuBus;
use crate::state::{CpuState, FLAG_DF, FLAG_ZF};
use aero_x86::{DecodedInst, Instruction, Mnemonic, OpKind, Register};

const BULK_THRESHOLD_BYTES: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RepMode {
    None,
    Rep,
    Repe,
    Repne,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AddrSize {
    A16,
    A32,
    A64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StringOp {
    Movs,
    Stos,
    Lods,
    Cmps,
    Scas,
}

pub fn handles(instr: &Instruction) -> bool {
    let is_string_mnemonic = matches!(
        instr.mnemonic(),
        Mnemonic::Movsb
            | Mnemonic::Movsw
            | Mnemonic::Movsd
            | Mnemonic::Movsq
            | Mnemonic::Stosb
            | Mnemonic::Stosw
            | Mnemonic::Stosd
            | Mnemonic::Stosq
            | Mnemonic::Lodsb
            | Mnemonic::Lodsw
            | Mnemonic::Lodsd
            | Mnemonic::Lodsq
            | Mnemonic::Cmpsb
            | Mnemonic::Cmpsw
            | Mnemonic::Cmpsd
            | Mnemonic::Cmpsq
            | Mnemonic::Scasb
            | Mnemonic::Scasw
            | Mnemonic::Scasd
            | Mnemonic::Scasq
    );
    if !is_string_mnemonic {
        return false;
    }

    // Some mnemonics overlap with SSE (e.g. MOVSD/CMPSD). Filter out encodings
    // that use registers Tier-0 doesn't model for string semantics (XMM/YMM/etc).
    for i in 0..instr.op_count() {
        if instr.op_kind(i) != OpKind::Register {
            continue;
        }
        let reg = instr.op_register(i);
        if reg == Register::None {
            continue;
        }
        if super::ops_data::reg_bits(reg).is_err() {
            return false;
        }
    }

    true
}

pub fn exec<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    decoded: &DecodedInst,
    _next_ip: u64,
    addr_size_override: bool,
) -> Result<ExecOutcome, Exception> {
    let instr = &decoded.instr;
    let (op, elem_size) = string_op(instr.mnemonic())?;
    let addr_size = effective_addr_size(state, addr_size_override)?;
    let rep_mode = effective_rep_mode(op, instr);

    match op {
        StringOp::Movs => exec_movs(state, bus, instr, elem_size, addr_size, rep_mode)?,
        StringOp::Stos => exec_stos(state, bus, elem_size, addr_size, rep_mode)?,
        StringOp::Lods => exec_lods(state, bus, instr, elem_size, addr_size, rep_mode)?,
        StringOp::Cmps => exec_cmps(state, bus, instr, elem_size, addr_size, rep_mode)?,
        StringOp::Scas => exec_scas(state, bus, elem_size, addr_size, rep_mode)?,
    }

    Ok(ExecOutcome::Continue)
}

fn string_op(m: Mnemonic) -> Result<(StringOp, usize), Exception> {
    let (op, elem_size) = match m {
        Mnemonic::Movsb => (StringOp::Movs, 1),
        Mnemonic::Movsw => (StringOp::Movs, 2),
        Mnemonic::Movsd => (StringOp::Movs, 4),
        Mnemonic::Movsq => (StringOp::Movs, 8),
        Mnemonic::Stosb => (StringOp::Stos, 1),
        Mnemonic::Stosw => (StringOp::Stos, 2),
        Mnemonic::Stosd => (StringOp::Stos, 4),
        Mnemonic::Stosq => (StringOp::Stos, 8),
        Mnemonic::Lodsb => (StringOp::Lods, 1),
        Mnemonic::Lodsw => (StringOp::Lods, 2),
        Mnemonic::Lodsd => (StringOp::Lods, 4),
        Mnemonic::Lodsq => (StringOp::Lods, 8),
        Mnemonic::Cmpsb => (StringOp::Cmps, 1),
        Mnemonic::Cmpsw => (StringOp::Cmps, 2),
        Mnemonic::Cmpsd => (StringOp::Cmps, 4),
        Mnemonic::Cmpsq => (StringOp::Cmps, 8),
        Mnemonic::Scasb => (StringOp::Scas, 1),
        Mnemonic::Scasw => (StringOp::Scas, 2),
        Mnemonic::Scasd => (StringOp::Scas, 4),
        Mnemonic::Scasq => (StringOp::Scas, 8),
        _ => return Err(Exception::InvalidOpcode),
    };
    Ok((op, elem_size))
}

fn effective_rep_mode(op: StringOp, instr: &Instruction) -> RepMode {
    if !instr.has_rep_prefix() && !instr.has_repne_prefix() {
        return RepMode::None;
    }
    match op {
        StringOp::Cmps | StringOp::Scas => {
            if instr.has_repne_prefix() {
                RepMode::Repne
            } else {
                RepMode::Repe
            }
        }
        _ => RepMode::Rep,
    }
}

fn effective_addr_size(state: &CpuState, addr_size_override: bool) -> Result<AddrSize, Exception> {
    Ok(match state.bitness() {
        16 => {
            if addr_size_override {
                AddrSize::A32
            } else {
                AddrSize::A16
            }
        }
        32 => {
            if addr_size_override {
                AddrSize::A16
            } else {
                AddrSize::A32
            }
        }
        64 => {
            if addr_size_override {
                AddrSize::A32
            } else {
                AddrSize::A64
            }
        }
        _ => return Err(Exception::InvalidOpcode),
    })
}

fn count_reg(addr_size: AddrSize) -> Register {
    match addr_size {
        AddrSize::A16 => Register::CX,
        AddrSize::A32 => Register::ECX,
        AddrSize::A64 => Register::RCX,
    }
}

fn src_index_reg(addr_size: AddrSize) -> Register {
    match addr_size {
        AddrSize::A16 => Register::SI,
        AddrSize::A32 => Register::ESI,
        AddrSize::A64 => Register::RSI,
    }
}

fn dst_index_reg(addr_size: AddrSize) -> Register {
    match addr_size {
        AddrSize::A16 => Register::DI,
        AddrSize::A32 => Register::EDI,
        AddrSize::A64 => Register::RDI,
    }
}

fn read_count(state: &CpuState, addr_size: AddrSize) -> u64 {
    state.read_reg(count_reg(addr_size))
}

fn write_count(state: &mut CpuState, addr_size: AddrSize, value: u64) {
    state.write_reg(count_reg(addr_size), value);
}

fn read_si(state: &CpuState, addr_size: AddrSize) -> u64 {
    state.read_reg(src_index_reg(addr_size))
}

fn write_si(state: &mut CpuState, addr_size: AddrSize, value: u64) {
    state.write_reg(src_index_reg(addr_size), value);
}

fn read_di(state: &CpuState, addr_size: AddrSize) -> u64 {
    state.read_reg(dst_index_reg(addr_size))
}

fn write_di(state: &mut CpuState, addr_size: AddrSize, value: u64) {
    state.write_reg(dst_index_reg(addr_size), value);
}

fn step_delta(state: &CpuState, elem_size: usize) -> i64 {
    if state.get_flag(FLAG_DF) {
        -(elem_size as i64)
    } else {
        elem_size as i64
    }
}

fn add_wrapping(value: u64, delta: i64, addr_size: AddrSize) -> u64 {
    match addr_size {
        AddrSize::A16 => {
            let v = value as u16;
            let d = delta as i32;
            v.wrapping_add(d as u16) as u64
        }
        AddrSize::A32 => {
            let v = value as u32;
            let d = delta as i64;
            v.wrapping_add(d as u32) as u64
        }
        AddrSize::A64 => value.wrapping_add(delta as u64),
    }
}

fn advance_n(value: u64, elem_size: usize, count: u64, df: bool, addr_size: AddrSize) -> u64 {
    let total = (elem_size as u64).wrapping_mul(count);
    match addr_size {
        AddrSize::A16 => {
            let v = value as u16;
            let t = total as u16;
            if df {
                v.wrapping_sub(t) as u64
            } else {
                v.wrapping_add(t) as u64
            }
        }
        AddrSize::A32 => {
            let v = value as u32;
            let t = total as u32;
            if df {
                v.wrapping_sub(t) as u64
            } else {
                v.wrapping_add(t) as u64
            }
        }
        AddrSize::A64 => {
            if df {
                value.wrapping_sub(total)
            } else {
                value.wrapping_add(total)
            }
        }
    }
}

fn addr_mask(addr_size: AddrSize) -> u64 {
    match addr_size {
        AddrSize::A16 => 0xFFFF,
        AddrSize::A32 => 0xFFFF_FFFF,
        AddrSize::A64 => u64::MAX,
    }
}

fn offsets_contiguous_without_wrap(offset: u64, count: u64, elem_size: usize, df: bool, addr_size: AddrSize) -> bool {
    // Address-size wrapping means offsets are only contiguous in linear memory if they do not wrap
    // within the repeated range. Only check this for 16/32-bit address sizes (64-bit wrap is
    // effectively impossible in practice).
    if matches!(addr_size, AddrSize::A64) {
        return true;
    }
    if count == 0 {
        return true;
    }

    let span = match count
        .checked_sub(1)
        .and_then(|c| c.checked_mul(elem_size as u64))
    {
        Some(v) => v,
        None => return false,
    };

    if df {
        offset >= span
    } else {
        let end = match offset.checked_add(span) {
            Some(v) => v,
            None => return false,
        };
        end <= addr_mask(addr_size)
    }
}

fn src_segment(instr: &Instruction) -> Register {
    let seg = instr.segment_prefix();
    if seg == Register::None {
        Register::DS
    } else {
        seg
    }
}

fn linear(state: &CpuState, seg: Register, offset: u64) -> u64 {
    state.seg_base_reg(seg).wrapping_add(offset)
}

fn read_mem<B: CpuBus>(bus: &mut B, addr: u64, size: usize) -> Result<u64, Exception> {
    match size {
        1 => Ok(bus.read_u8(addr)? as u64),
        2 => Ok(bus.read_u16(addr)? as u64),
        4 => Ok(bus.read_u32(addr)? as u64),
        8 => Ok(bus.read_u64(addr)?),
        _ => Err(Exception::InvalidOpcode),
    }
}

fn write_mem<B: CpuBus>(bus: &mut B, addr: u64, size: usize, value: u64) -> Result<(), Exception> {
    match size {
        1 => bus.write_u8(addr, value as u8),
        2 => bus.write_u16(addr, value as u16),
        4 => bus.write_u32(addr, value as u32),
        8 => bus.write_u64(addr, value),
        _ => Err(Exception::InvalidOpcode),
    }
}

fn stos_pattern(state: &CpuState, size: usize) -> Result<[u8; 8], Exception> {
    let value = match size {
        1 => state.read_reg(Register::AL),
        2 => state.read_reg(Register::AX),
        4 => state.read_reg(Register::EAX),
        8 => state.read_reg(Register::RAX),
        _ => return Err(Exception::InvalidOpcode),
    };
    Ok(value.to_le_bytes())
}

fn exec_movs<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    elem_size: usize,
    addr_size: AddrSize,
    rep_mode: RepMode,
) -> Result<(), Exception> {
    let delta = step_delta(state, elem_size);
    let df = state.get_flag(FLAG_DF);

    let mut count = match rep_mode {
        RepMode::None => 1,
        RepMode::Rep | RepMode::Repe | RepMode::Repne => read_count(state, addr_size),
    };
    if rep_mode != RepMode::None && count == 0 {
        return Ok(());
    }

    let src_seg = src_segment(instr);

    // REP MOVS* fast path.
    if rep_mode != RepMode::None && bus.supports_bulk_copy() {
        let si = read_si(state, addr_size);
        let di = read_di(state, addr_size);

        if offsets_contiguous_without_wrap(si, count, elem_size, df, addr_size)
            && offsets_contiguous_without_wrap(di, count, elem_size, df, addr_size)
        {
            if let Some(total_bytes_u64) = (elem_size as u64).checked_mul(count) {
                // `CpuBus::bulk_copy` takes a `usize` length, so only use it when the total size fits.
                // This avoids truncation bugs on 32-bit hosts (e.g. wasm32).
                if total_bytes_u64 >= BULK_THRESHOLD_BYTES as u64 && total_bytes_u64 <= usize::MAX as u64 {
                    let back_count = count.saturating_sub(1);
                    let src_offset = if df {
                        advance_n(si, elem_size, back_count, true, addr_size)
                    } else {
                        si
                    };
                    let dst_offset = if df {
                        advance_n(di, elem_size, back_count, true, addr_size)
                    } else {
                        di
                    };

                    let src_start = linear(state, src_seg, src_offset);
                    let dst_start = linear(state, Register::ES, dst_offset);

                    if let (Some(src_end), Some(dst_end)) = (
                        src_start.checked_add(total_bytes_u64),
                        dst_start.checked_add(total_bytes_u64),
                    ) {
                        let overlap = src_start < dst_end && dst_start < src_end;
                        let hazard = if !overlap {
                            false
                        } else if !df {
                            // DF=0 copies low->high. Hazard when destination starts inside source at a higher
                            // address.
                            src_start < dst_start && dst_start < src_end
                        } else {
                            // DF=1 copies high->low. Hazard when source starts inside destination at a higher
                            // address.
                            dst_start < src_start && src_start < dst_end
                        };

                        if !hazard && bus.bulk_copy(dst_start, src_start, total_bytes_u64 as usize)? {
                            let si_new = advance_n(si, elem_size, count, df, addr_size);
                            let di_new = advance_n(di, elem_size, count, df, addr_size);
                            write_si(state, addr_size, si_new);
                            write_di(state, addr_size, di_new);
                            write_count(state, addr_size, 0);
                            return Ok(());
                        }
                    }
                }
            }
        }
    }

    while count != 0 {
        let si = read_si(state, addr_size);
        let di = read_di(state, addr_size);
        let src_addr = linear(state, src_seg, si);
        let dst_addr = linear(state, Register::ES, di);

        let value = read_mem(bus, src_addr, elem_size)?;
        write_mem(bus, dst_addr, elem_size, value)?;

        let si_new = add_wrapping(si, delta, addr_size);
        let di_new = add_wrapping(di, delta, addr_size);
        write_si(state, addr_size, si_new);
        write_di(state, addr_size, di_new);

        if rep_mode != RepMode::None {
            count -= 1;
            write_count(state, addr_size, count);
        } else {
            break;
        }
    }

    Ok(())
}

fn exec_stos<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    elem_size: usize,
    addr_size: AddrSize,
    rep_mode: RepMode,
) -> Result<(), Exception> {
    let delta = step_delta(state, elem_size);
    let df = state.get_flag(FLAG_DF);

    let mut count = match rep_mode {
        RepMode::None => 1,
        RepMode::Rep | RepMode::Repe | RepMode::Repne => read_count(state, addr_size),
    };
    if rep_mode != RepMode::None && count == 0 {
        return Ok(());
    }

    // REP STOS* fast path.
    if rep_mode != RepMode::None && bus.supports_bulk_set() {
        let di = read_di(state, addr_size);

        if offsets_contiguous_without_wrap(di, count, elem_size, df, addr_size) {
            if let Some(total_bytes_u64) = (elem_size as u64).checked_mul(count) {
                if total_bytes_u64 >= BULK_THRESHOLD_BYTES as u64
                    && total_bytes_u64 <= usize::MAX as u64
                    && count <= usize::MAX as u64
                {
                    let back_count = count.saturating_sub(1);
                    let dst_offset = if df {
                        advance_n(di, elem_size, back_count, true, addr_size)
                    } else {
                        di
                    };

                    let dst_start = linear(state, Register::ES, dst_offset);

                    let pattern = stos_pattern(state, elem_size)?;
                    if bus.bulk_set(dst_start, &pattern[..elem_size], count as usize)? {
                        let di_new = advance_n(di, elem_size, count, df, addr_size);
                        write_di(state, addr_size, di_new);
                        write_count(state, addr_size, 0);
                        return Ok(());
                    }
                }
            }
        }
    }

    while count != 0 {
        let di = read_di(state, addr_size);
        let dst_addr = linear(state, Register::ES, di);

        let value = match elem_size {
            1 => state.read_reg(Register::AL),
            2 => state.read_reg(Register::AX),
            4 => state.read_reg(Register::EAX),
            8 => state.read_reg(Register::RAX),
            _ => return Err(Exception::InvalidOpcode),
        };
        write_mem(bus, dst_addr, elem_size, value)?;

        let di_new = add_wrapping(di, delta, addr_size);
        write_di(state, addr_size, di_new);

        if rep_mode != RepMode::None {
            count -= 1;
            write_count(state, addr_size, count);
        } else {
            break;
        }
    }

    Ok(())
}

fn exec_lods<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    elem_size: usize,
    addr_size: AddrSize,
    rep_mode: RepMode,
) -> Result<(), Exception> {
    let delta = step_delta(state, elem_size);

    let mut count = match rep_mode {
        RepMode::None => 1,
        RepMode::Rep | RepMode::Repe | RepMode::Repne => read_count(state, addr_size),
    };
    if rep_mode != RepMode::None && count == 0 {
        return Ok(());
    }

    let src_seg = src_segment(instr);

    while count != 0 {
        let si = read_si(state, addr_size);
        let src_addr = linear(state, src_seg, si);
        let value = read_mem(bus, src_addr, elem_size)?;

        match elem_size {
            1 => state.write_reg(Register::AL, value),
            2 => state.write_reg(Register::AX, value),
            4 => state.write_reg(Register::EAX, value),
            8 => state.write_reg(Register::RAX, value),
            _ => return Err(Exception::InvalidOpcode),
        }

        let si_new = add_wrapping(si, delta, addr_size);
        write_si(state, addr_size, si_new);

        if rep_mode != RepMode::None {
            count -= 1;
            write_count(state, addr_size, count);
        } else {
            break;
        }
    }

    Ok(())
}

fn exec_cmps<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    elem_size: usize,
    addr_size: AddrSize,
    rep_mode: RepMode,
) -> Result<(), Exception> {
    let delta = step_delta(state, elem_size);

    let mut count = match rep_mode {
        RepMode::None => 1,
        RepMode::Rep | RepMode::Repe | RepMode::Repne => read_count(state, addr_size),
    };
    if rep_mode != RepMode::None && count == 0 {
        return Ok(());
    }

    let src_seg = src_segment(instr);

    while count != 0 {
        let si = read_si(state, addr_size);
        let di = read_di(state, addr_size);
        let src_addr = linear(state, src_seg, si);
        let dst_addr = linear(state, Register::ES, di);

        let src_val = read_mem(bus, src_addr, elem_size)?;
        let dst_val = read_mem(bus, dst_addr, elem_size)?;

        // CMPS sets flags as if computing SRC - DEST.
        let (_res, flags) =
            super::ops_alu::sub_with_flags(state, src_val, dst_val, 0, (elem_size * 8) as u32);
        state.set_rflags(flags);

        let si_new = add_wrapping(si, delta, addr_size);
        let di_new = add_wrapping(di, delta, addr_size);
        write_si(state, addr_size, si_new);
        write_di(state, addr_size, di_new);

        if rep_mode != RepMode::None {
            count -= 1;
            write_count(state, addr_size, count);
            match rep_mode {
                RepMode::Rep => {}
                RepMode::Repe => {
                    if !state.get_flag(FLAG_ZF) {
                        break;
                    }
                }
                RepMode::Repne => {
                    if state.get_flag(FLAG_ZF) {
                        break;
                    }
                }
                RepMode::None => unreachable!(),
            }
        } else {
            break;
        }
    }

    Ok(())
}

fn exec_scas<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    elem_size: usize,
    addr_size: AddrSize,
    rep_mode: RepMode,
) -> Result<(), Exception> {
    let delta = step_delta(state, elem_size);

    let mut count = match rep_mode {
        RepMode::None => 1,
        RepMode::Rep | RepMode::Repe | RepMode::Repne => read_count(state, addr_size),
    };
    if rep_mode != RepMode::None && count == 0 {
        return Ok(());
    }

    while count != 0 {
        let di = read_di(state, addr_size);
        let mem_addr = linear(state, Register::ES, di);
        let mem_val = read_mem(bus, mem_addr, elem_size)?;

        let acc_val = match elem_size {
            1 => state.read_reg(Register::AL),
            2 => state.read_reg(Register::AX),
            4 => state.read_reg(Register::EAX),
            8 => state.read_reg(Register::RAX),
            _ => return Err(Exception::InvalidOpcode),
        };
        let (_res, flags) =
            super::ops_alu::sub_with_flags(state, acc_val, mem_val, 0, (elem_size * 8) as u32);
        state.set_rflags(flags);

        let di_new = add_wrapping(di, delta, addr_size);
        write_di(state, addr_size, di_new);

        if rep_mode != RepMode::None {
            count -= 1;
            write_count(state, addr_size, count);
            match rep_mode {
                RepMode::Rep => {}
                RepMode::Repe => {
                    if !state.get_flag(FLAG_ZF) {
                        break;
                    }
                }
                RepMode::Repne => {
                    if state.get_flag(FLAG_ZF) {
                        break;
                    }
                }
                RepMode::None => unreachable!(),
            }
        } else {
            break;
        }
    }

    Ok(())
}
