use super::ExecOutcome;
use crate::exception::{AssistReason, Exception};
use crate::mem::CpuBus;
use crate::state::{mask_bits, CpuState};
use aero_x86::{DecodedInst, Instruction, MemorySize, Mnemonic, OpKind, Register};

pub fn handles_mnemonic(m: Mnemonic) -> bool {
    matches!(
        m,
        Mnemonic::Mov
            | Mnemonic::Lea
            | Mnemonic::Xchg
            | Mnemonic::Movsx
            | Mnemonic::Movzx
            | Mnemonic::Bswap
            | Mnemonic::Xadd
    ) || is_cmov(m)
        || is_setcc(m)
}

pub fn exec<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    decoded: &DecodedInst,
    next_ip: u64,
) -> Result<ExecOutcome, Exception> {
    let instr = &decoded.instr;
    match instr.mnemonic() {
        Mnemonic::Mov => {
            if instr.has_lock_prefix() {
                return Err(Exception::InvalidOpcode);
            }
            if instr.op_kind(0) == OpKind::Register && is_ctrl_or_debug_reg(instr.op0_register())
                || instr.op_kind(1) == OpKind::Register
                    && is_ctrl_or_debug_reg(instr.op1_register())
            {
                // MOV to/from control/debug registers is privileged (and updates CPU mode/paging).
                return Ok(ExecOutcome::Assist(AssistReason::Privileged));
            }

            if state.mode != crate::state::CpuMode::Bit16
                && instr.op_kind(0) == OpKind::Register
                && is_seg_reg(instr.op0_register())
            {
                // Segment loads in protected/long mode require descriptor lookups and can fault.
                // Delegate to the runtime assist layer.
                return Ok(ExecOutcome::Assist(AssistReason::Privileged));
            }
            let v = read_op(state, bus, instr, 1, next_ip)?;
            write_op(state, bus, instr, 0, v, next_ip)?;
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Lea => {
            if instr.has_lock_prefix() {
                return Err(Exception::InvalidOpcode);
            }
            let addr = calc_ea(state, instr, next_ip, false)?;
            write_op(state, bus, instr, 0, addr, next_ip)?;
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Xchg => {
            let lock = instr.has_lock_prefix();
            let op0_kind = instr.op_kind(0);
            let op1_kind = instr.op_kind(1);

            if op0_kind == OpKind::Register && op1_kind == OpKind::Register {
                if lock {
                    return Err(Exception::InvalidOpcode);
                }
                let a = read_op(state, bus, instr, 0, next_ip)?;
                let b = read_op(state, bus, instr, 1, next_ip)?;
                write_op(state, bus, instr, 0, b, next_ip)?;
                write_op(state, bus, instr, 1, a, next_ip)?;
            } else {
                // XCHG with a memory operand is implicitly atomic (acts as if `LOCK` is present).
                let (mem_op, reg_op) = match (op0_kind, op1_kind) {
                    (OpKind::Memory, OpKind::Register) => (0usize, 1usize),
                    (OpKind::Register, OpKind::Memory) => (1usize, 0usize),
                    _ => return Err(Exception::InvalidOpcode),
                };

                let bits = op_bits(state, instr, reg_op)?;
                let reg = instr.op_register(reg_op as u32);
                let reg_val = state.read_reg(reg) & mask_bits(bits);
                let addr = calc_ea(state, instr, next_ip, true)?;

                let old = super::atomic_rmw_sized(bus, addr, bits, |old| (reg_val, old))?;
                state.write_reg(reg, old);

                // `mem_op` is unused because iced-x86 stores the memory operand separately
                // from the operand index; both encodings address the same `memory_*` fields.
                let _ = mem_op;
            }
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Movsx | Mnemonic::Movzx => {
            if instr.has_lock_prefix() {
                return Err(Exception::InvalidOpcode);
            }
            let src_bits = op_bits(state, instr, 1)?;
            let dst_bits = op_bits(state, instr, 0)?;
            let src = read_op_sized(state, bus, instr, 1, src_bits, next_ip)?;
            let v = if instr.mnemonic() == Mnemonic::Movsx {
                sign_extend(src, src_bits, dst_bits)
            } else {
                src & mask_bits(dst_bits)
            };
            write_op_sized(state, bus, instr, 0, v, dst_bits, next_ip)?;
            Ok(ExecOutcome::Continue)
        }
        m if is_cmov(m) => {
            if instr.has_lock_prefix() {
                return Err(Exception::InvalidOpcode);
            }
            if eval_cond(state, m) {
                let v = read_op(state, bus, instr, 1, next_ip)?;
                write_op(state, bus, instr, 0, v, next_ip)?;
            }
            Ok(ExecOutcome::Continue)
        }
        m if is_setcc(m) => {
            if instr.has_lock_prefix() {
                return Err(Exception::InvalidOpcode);
            }
            let v = if eval_cond(state, m) { 1u64 } else { 0u64 };
            write_op_sized(state, bus, instr, 0, v, 8, next_ip)?;
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Bswap => {
            if instr.has_lock_prefix() {
                return Err(Exception::InvalidOpcode);
            }
            let reg = instr.op0_register();
            let bits = reg_bits(reg)?;
            let v = state.read_reg(reg);
            let swapped = match bits {
                32 => (v as u32).swap_bytes() as u64,
                64 => v.swap_bytes(),
                _ => return Err(Exception::InvalidOpcode),
            };
            state.write_reg(reg, swapped);
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Xadd => {
            if instr.has_lock_prefix() && instr.op_kind(0) == OpKind::Register {
                return Err(Exception::InvalidOpcode);
            }
            let bits = op_bits(state, instr, 0)?;
            let src = read_op_sized(state, bus, instr, 1, bits, next_ip)?;
            let (dst, flags) = if instr.has_lock_prefix() && instr.op_kind(0) == OpKind::Memory {
                let addr = calc_ea(state, instr, next_ip, true)?;
                let dst = super::atomic_rmw_sized(bus, addr, bits, |old| {
                    let new = old.wrapping_add(src) & mask_bits(bits);
                    (new, old)
                })?;
                let (_, flags) = super::ops_alu::add_with_flags(state, dst, src, 0, bits);
                (dst, flags)
            } else {
                let dst = read_op_sized(state, bus, instr, 0, bits, next_ip)?;
                let (res, flags) = super::ops_alu::add_with_flags(state, dst, src, 0, bits);
                write_op_sized(state, bus, instr, 0, res, bits, next_ip)?;
                (dst, flags)
            };
            write_op_sized(state, bus, instr, 1, dst, bits, next_ip)?;
            state.set_rflags(flags);
            Ok(ExecOutcome::Continue)
        }
        _ => Err(Exception::InvalidOpcode),
    }
}

pub(crate) fn eval_cond(state: &CpuState, m: Mnemonic) -> bool {
    use crate::state::{FLAG_CF, FLAG_OF, FLAG_PF, FLAG_SF, FLAG_ZF};
    let cf = state.get_flag(FLAG_CF);
    let pf = state.get_flag(FLAG_PF);
    let zf = state.get_flag(FLAG_ZF);
    let sf = state.get_flag(FLAG_SF);
    let of = state.get_flag(FLAG_OF);
    match m {
        Mnemonic::Jo | Mnemonic::Cmovo | Mnemonic::Seto => of,
        Mnemonic::Jno | Mnemonic::Cmovno | Mnemonic::Setno => !of,
        Mnemonic::Jb | Mnemonic::Cmovb | Mnemonic::Setb => cf,
        Mnemonic::Jae | Mnemonic::Cmovae | Mnemonic::Setae => !cf,
        Mnemonic::Je | Mnemonic::Cmove | Mnemonic::Sete => zf,
        Mnemonic::Jne | Mnemonic::Cmovne | Mnemonic::Setne => !zf,
        Mnemonic::Jbe | Mnemonic::Cmovbe | Mnemonic::Setbe => cf || zf,
        Mnemonic::Ja | Mnemonic::Cmova | Mnemonic::Seta => !cf && !zf,
        Mnemonic::Js | Mnemonic::Cmovs | Mnemonic::Sets => sf,
        Mnemonic::Jns | Mnemonic::Cmovns | Mnemonic::Setns => !sf,
        Mnemonic::Jp | Mnemonic::Cmovp | Mnemonic::Setp => pf,
        Mnemonic::Jnp | Mnemonic::Cmovnp | Mnemonic::Setnp => !pf,
        Mnemonic::Jl | Mnemonic::Cmovl | Mnemonic::Setl => sf != of,
        Mnemonic::Jge | Mnemonic::Cmovge | Mnemonic::Setge => sf == of,
        Mnemonic::Jle | Mnemonic::Cmovle | Mnemonic::Setle => zf || (sf != of),
        Mnemonic::Jg | Mnemonic::Cmovg | Mnemonic::Setg => !zf && (sf == of),
        _ => false,
    }
}

fn is_cmov(m: Mnemonic) -> bool {
    matches!(
        m,
        Mnemonic::Cmovo
            | Mnemonic::Cmovno
            | Mnemonic::Cmovb
            | Mnemonic::Cmovae
            | Mnemonic::Cmove
            | Mnemonic::Cmovne
            | Mnemonic::Cmovbe
            | Mnemonic::Cmova
            | Mnemonic::Cmovs
            | Mnemonic::Cmovns
            | Mnemonic::Cmovp
            | Mnemonic::Cmovnp
            | Mnemonic::Cmovl
            | Mnemonic::Cmovge
            | Mnemonic::Cmovle
            | Mnemonic::Cmovg
    )
}

fn is_setcc(m: Mnemonic) -> bool {
    matches!(
        m,
        Mnemonic::Seto
            | Mnemonic::Setno
            | Mnemonic::Setb
            | Mnemonic::Setae
            | Mnemonic::Sete
            | Mnemonic::Setne
            | Mnemonic::Setbe
            | Mnemonic::Seta
            | Mnemonic::Sets
            | Mnemonic::Setns
            | Mnemonic::Setp
            | Mnemonic::Setnp
            | Mnemonic::Setl
            | Mnemonic::Setge
            | Mnemonic::Setle
            | Mnemonic::Setg
    )
}

fn sign_extend(val: u64, from_bits: u32, to_bits: u32) -> u64 {
    let masked = val & mask_bits(from_bits);
    if from_bits == 64 {
        masked
    } else {
        let sign_bit = 1u64 << (from_bits - 1);
        let extended = if (masked & sign_bit) != 0 {
            masked | (!0u64 << from_bits)
        } else {
            masked
        };
        extended & mask_bits(to_bits)
    }
}

pub(crate) fn reg_bits(reg: Register) -> Result<u32, Exception> {
    use Register::*;
    let bits = match reg {
        AL | CL | DL | BL | AH | CH | DH | BH | SPL | BPL | SIL | DIL | R8L | R9L | R10L | R11L
        | R12L | R13L | R14L | R15L => 8,
        AX | CX | DX | BX | SP | BP | SI | DI | R8W | R9W | R10W | R11W | R12W | R13W | R14W
        | R15W => 16,
        EAX | ECX | EDX | EBX | ESP | EBP | ESI | EDI | R8D | R9D | R10D | R11D | R12D | R13D
        | R14D | R15D => 32,
        RAX | RCX | RDX | RBX | RSP | RBP | RSI | RDI | R8 | R9 | R10 | R11 | R12 | R13 | R14
        | R15 => 64,
        ES | CS | SS | DS | FS | GS => 16,
        _ => return Err(Exception::InvalidOpcode),
    };
    Ok(bits)
}

fn is_seg_reg(reg: Register) -> bool {
    matches!(
        reg,
        Register::ES | Register::CS | Register::SS | Register::DS | Register::FS | Register::GS
    )
}

fn is_ctrl_or_debug_reg(reg: Register) -> bool {
    matches!(
        reg,
        Register::CR0
            | Register::CR2
            | Register::CR3
            | Register::CR4
            | Register::CR8
            | Register::DR0
            | Register::DR1
            | Register::DR2
            | Register::DR3
            | Register::DR6
            | Register::DR7
    )
}

pub(crate) fn op_bits(_state: &CpuState, instr: &Instruction, op: usize) -> Result<u32, Exception> {
    match instr.op_kind(op as u32) {
        OpKind::Register => reg_bits(instr.op_register(op as u32)),
        OpKind::Memory => mem_bits(instr),
        OpKind::Immediate8 => Ok(8),
        OpKind::Immediate16 => Ok(16),
        OpKind::Immediate32 => Ok(32),
        OpKind::Immediate64 => Ok(64),
        OpKind::Immediate8to16 => Ok(16),
        OpKind::Immediate8to32 => Ok(32),
        OpKind::Immediate8to64 => Ok(64),
        OpKind::Immediate32to64 => Ok(64),
        _ => Err(Exception::InvalidOpcode),
    }
}

fn mem_bits(instr: &Instruction) -> Result<u32, Exception> {
    let bits = match instr.memory_size() {
        MemorySize::UInt8 | MemorySize::Int8 => 8,
        MemorySize::UInt16 | MemorySize::Int16 => 16,
        MemorySize::UInt32 | MemorySize::Int32 => 32,
        MemorySize::UInt64 | MemorySize::Int64 => 64,
        _ => return Err(Exception::InvalidOpcode),
    };
    Ok(bits)
}

pub(crate) fn read_op<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    op: usize,
    next_ip: u64,
) -> Result<u64, Exception> {
    let bits = op_bits(state, instr, op)?;
    read_op_sized(state, bus, instr, op, bits, next_ip)
}

pub(crate) fn read_op_sized<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    op: usize,
    bits: u32,
    next_ip: u64,
) -> Result<u64, Exception> {
    match instr.op_kind(op as u32) {
        OpKind::Register => Ok(state.read_reg(instr.op_register(op as u32)) & mask_bits(bits)),
        OpKind::Memory => {
            let addr = calc_ea(state, instr, next_ip, true)?;
            read_mem(bus, addr, bits)
        }
        OpKind::Immediate8 => Ok(instr.immediate8() as u64 & mask_bits(bits)),
        OpKind::Immediate16 => Ok(instr.immediate16() as u64 & mask_bits(bits)),
        OpKind::Immediate32 => Ok(instr.immediate32() as u64 & mask_bits(bits)),
        OpKind::Immediate64 => Ok(instr.immediate64() & mask_bits(bits)),
        OpKind::Immediate8to16 => Ok(instr.immediate8to16() as u64 & mask_bits(bits)),
        OpKind::Immediate8to32 => Ok(instr.immediate8to32() as u64 & mask_bits(bits)),
        OpKind::Immediate8to64 => Ok((instr.immediate8to64() as u64) & mask_bits(bits)),
        OpKind::Immediate32to64 => Ok((instr.immediate32to64() as u64) & mask_bits(bits)),
        _ => Err(Exception::InvalidOpcode),
    }
}

pub(crate) fn write_op<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    op: usize,
    val: u64,
    next_ip: u64,
) -> Result<(), Exception> {
    let bits = op_bits(state, instr, op)?;
    write_op_sized(state, bus, instr, op, val, bits, next_ip)
}

pub(crate) fn write_op_sized<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    op: usize,
    val: u64,
    bits: u32,
    next_ip: u64,
) -> Result<(), Exception> {
    let v = val & mask_bits(bits);
    match instr.op_kind(op as u32) {
        OpKind::Register => {
            state.write_reg(instr.op_register(op as u32), v);
            Ok(())
        }
        OpKind::Memory => {
            let addr = calc_ea(state, instr, next_ip, true)?;
            write_mem(bus, addr, bits, v)
        }
        _ => Err(Exception::InvalidOpcode),
    }
}

pub(crate) fn calc_ea(
    state: &CpuState,
    instr: &Instruction,
    next_ip: u64,
    include_seg: bool,
) -> Result<u64, Exception> {
    let base = instr.memory_base();
    let index = instr.memory_index();
    let scale = instr.memory_index_scale() as u64;
    // iced-x86 encodes RIP-relative displacements as an *absolute* address:
    //   absolute = next_ip + disp32
    //
    // The rest of the EA calculation logic treats `memory_displacement64()` as
    // the raw displacement, so we normalize it back to disp32 when the base is
    // RIP. This keeps the address computation uniform and prevents double-
    // adding `next_ip` for RIP-relative memory operands.
    let mut disp = instr.memory_displacement64() as i128;
    if base == Register::RIP {
        disp -= next_ip as i128;
    }

    let addr_bits = if base == Register::RIP {
        64
    } else if base != Register::None {
        reg_bits(base)?
    } else if index != Register::None {
        reg_bits(index)?
    } else {
        match instr.memory_displ_size() {
            2 => 16,
            4 => 32,
            8 => 64,
            _ => state.bitness(),
        }
    };

    let mut offset: i128 = disp;
    if base != Register::None {
        let base_val = if base == Register::RIP {
            next_ip
        } else {
            state.read_reg(base)
        };
        offset += (base_val & mask_bits(addr_bits)) as i128;
    }
    if index != Register::None {
        let idx_val = state.read_reg(index) & mask_bits(addr_bits);
        offset += (idx_val as i128) * (scale as i128);
    }

    let addr = (offset as u64) & mask_bits(addr_bits);
    if include_seg {
        Ok(state
            .seg_base_reg(instr.memory_segment())
            .wrapping_add(addr))
    } else {
        Ok(addr)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{CpuMode, CpuState};

    #[test]
    fn calc_ea_rip_relative_does_not_double_add_next_ip() {
        // mov rax, qword ptr [rip+0x12345678]
        let bytes = [0x48, 0x8B, 0x05, 0x78, 0x56, 0x34, 0x12];
        let ip = 0x1000u64;
        let decoded = aero_x86::decode(&bytes, ip, 64).expect("decode");
        let next_ip = ip + decoded.len as u64;

        let state = CpuState::new(CpuMode::Bit64);
        let addr = calc_ea(&state, &decoded.instr, next_ip, false).expect("calc_ea");

        // next_ip + disp32
        assert_eq!(addr, next_ip.wrapping_add(0x12345678));
    }
}

pub(crate) fn read_mem<B: CpuBus>(bus: &mut B, addr: u64, bits: u32) -> Result<u64, Exception> {
    match bits {
        8 => Ok(bus.read_u8(addr)? as u64),
        16 => Ok(bus.read_u16(addr)? as u64),
        32 => Ok(bus.read_u32(addr)? as u64),
        64 => Ok(bus.read_u64(addr)?),
        _ => Err(Exception::InvalidOpcode),
    }
}

pub(crate) fn write_mem<B: CpuBus>(
    bus: &mut B,
    addr: u64,
    bits: u32,
    val: u64,
) -> Result<(), Exception> {
    match bits {
        8 => bus.write_u8(addr, val as u8),
        16 => bus.write_u16(addr, val as u16),
        32 => bus.write_u32(addr, val as u32),
        64 => bus.write_u64(addr, val),
        _ => Err(Exception::InvalidOpcode),
    }
}
