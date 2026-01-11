use crate::bus::Bus;
use crate::cpu::{Cpu, CpuMode, RFlags, Segment};
use crate::interp::decode::PrefixState;
use crate::interp::{DecodedInst, ExecError, InstKind};
use crate::Exception;

use super::alu;

#[derive(Clone, Copy, Debug)]
pub struct ModRm {
    pub mod_bits: u8,
    pub reg: u8,
    pub rm: u8,
}

#[derive(Clone, Copy, Debug)]
pub struct Sib {
    pub scale: u8,
    pub index: u8,
    pub base: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AluOp {
    Add,
    Or,
    Adc,
    Sbb,
    And,
    Sub,
    Xor,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnaryOp {
    Inc,
    Dec,
    Not,
    Neg,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BitOp {
    Bts,
    Btr,
    Btc,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BitIndex {
    Reg(u8),
    Imm(u8),
}

#[derive(Clone, Debug)]
pub enum AtomicKind {
    CmpXchg,
    CmpXchg8B,
    CmpXchg16B,
    Xadd,
    Xchg,
    AluRmReg(AluOp),
    AluRmImm { op: AluOp, imm: i64 },
    Unary(UnaryOp),
    BitOp { op: BitOp, index: BitIndex },
}

#[derive(Clone, Debug)]
pub struct DecodedAtomicInst {
    pub len: usize,
    pub prefixes: PrefixState,
    pub kind: AtomicKind,
    pub size: usize,
    pub modrm: ModRm,
    pub sib: Option<Sib>,
    pub disp: i32,
}

pub fn decode_atomics(
    mode: CpuMode,
    bytes: &[u8],
    idx: usize,
    opcode0: u8,
    prefixes: PrefixState,
) -> Result<DecodedInst, ExecError> {
    let mut cursor = idx;

    let (kind, size, modrm, sib, disp) = match opcode0 {
        0x0F => {
            let opcode1 = *bytes.get(cursor).ok_or(ExecError::TruncatedInstruction)?;
            cursor += 1;
            match opcode1 {
                0xB0 => {
                    let (modrm, sib, disp, used) = decode_modrm(bytes, cursor, &prefixes, mode)?;
                    cursor += used;
                    (AtomicKind::CmpXchg, 1, modrm, sib, disp)
                }
                0xB1 => {
                    let (modrm, sib, disp, used) = decode_modrm(bytes, cursor, &prefixes, mode)?;
                    cursor += used;
                    (
                        AtomicKind::CmpXchg,
                        operand_size_bytes(mode, &prefixes),
                        modrm,
                        sib,
                        disp,
                    )
                }
                0xC1 => {
                    let (modrm, sib, disp, used) = decode_modrm(bytes, cursor, &prefixes, mode)?;
                    cursor += used;
                    (
                        AtomicKind::Xadd,
                        operand_size_bytes(mode, &prefixes),
                        modrm,
                        sib,
                        disp,
                    )
                }
                0xC7 => {
                    let (modrm, sib, disp, used) = decode_modrm(bytes, cursor, &prefixes, mode)?;
                    cursor += used;
                    if modrm.reg != 1 {
                        return Err(ExecError::InvalidOpcode(opcode0));
                    }
                    match mode {
                        CpuMode::Long64 => (AtomicKind::CmpXchg16B, 16, modrm, sib, disp),
                        _ => (AtomicKind::CmpXchg8B, 8, modrm, sib, disp),
                    }
                }
                0xAB | 0xB3 | 0xBB => {
                    let (modrm, sib, disp, used) = decode_modrm(bytes, cursor, &prefixes, mode)?;
                    cursor += used;
                    let op = match opcode1 {
                        0xAB => BitOp::Bts,
                        0xB3 => BitOp::Btr,
                        _ => BitOp::Btc,
                    };
                    (
                        AtomicKind::BitOp {
                            op,
                            index: BitIndex::Reg(modrm.reg),
                        },
                        operand_size_bytes(mode, &prefixes),
                        modrm,
                        sib,
                        disp,
                    )
                }
                0xBA => {
                    let (modrm, sib, disp, used) = decode_modrm(bytes, cursor, &prefixes, mode)?;
                    cursor += used;
                    let imm = *bytes.get(cursor).ok_or(ExecError::TruncatedInstruction)?;
                    cursor += 1;
                    let op = match modrm.reg {
                        5 => BitOp::Bts,
                        6 => BitOp::Btr,
                        7 => BitOp::Btc,
                        _ => return Err(ExecError::InvalidOpcode(opcode0)),
                    };
                    (
                        AtomicKind::BitOp {
                            op,
                            index: BitIndex::Imm(imm),
                        },
                        operand_size_bytes(mode, &prefixes),
                        modrm,
                        sib,
                        disp,
                    )
                }
                _ => return Err(ExecError::InvalidOpcode(opcode0)),
            }
        }
        0x86 => {
            let (modrm, sib, disp, used) = decode_modrm(bytes, cursor, &prefixes, mode)?;
            cursor += used;
            (AtomicKind::Xchg, 1, modrm, sib, disp)
        }
        0x87 => {
            let (modrm, sib, disp, used) = decode_modrm(bytes, cursor, &prefixes, mode)?;
            cursor += used;
            (
                AtomicKind::Xchg,
                operand_size_bytes(mode, &prefixes),
                modrm,
                sib,
                disp,
            )
        }
        0x00 | 0x01 | 0x08 | 0x09 | 0x10 | 0x11 | 0x18 | 0x19 | 0x20 | 0x21 | 0x28 | 0x29
        | 0x30 | 0x31 => {
            let (op, size) = match opcode0 {
                0x00 => (AluOp::Add, 1),
                0x01 => (AluOp::Add, operand_size_bytes(mode, &prefixes)),
                0x08 => (AluOp::Or, 1),
                0x09 => (AluOp::Or, operand_size_bytes(mode, &prefixes)),
                0x10 => (AluOp::Adc, 1),
                0x11 => (AluOp::Adc, operand_size_bytes(mode, &prefixes)),
                0x18 => (AluOp::Sbb, 1),
                0x19 => (AluOp::Sbb, operand_size_bytes(mode, &prefixes)),
                0x20 => (AluOp::And, 1),
                0x21 => (AluOp::And, operand_size_bytes(mode, &prefixes)),
                0x28 => (AluOp::Sub, 1),
                0x29 => (AluOp::Sub, operand_size_bytes(mode, &prefixes)),
                0x30 => (AluOp::Xor, 1),
                _ => (AluOp::Xor, operand_size_bytes(mode, &prefixes)),
            };
            let (modrm, sib, disp, used) = decode_modrm(bytes, cursor, &prefixes, mode)?;
            cursor += used;
            (AtomicKind::AluRmReg(op), size, modrm, sib, disp)
        }
        0x80 | 0x81 | 0x83 => {
            let (modrm, sib, disp, used) = decode_modrm(bytes, cursor, &prefixes, mode)?;
            cursor += used;
            let op = match modrm.reg {
                0 => AluOp::Add,
                1 => AluOp::Or,
                2 => AluOp::Adc,
                3 => AluOp::Sbb,
                4 => AluOp::And,
                5 => AluOp::Sub,
                6 => AluOp::Xor,
                _ => return Err(ExecError::InvalidOpcode(opcode0)),
            };
            let size = match opcode0 {
                0x80 => 1,
                _ => operand_size_bytes(mode, &prefixes),
            };
            let imm = match opcode0 {
                0x80 => *bytes.get(cursor).ok_or(ExecError::TruncatedInstruction)? as i8 as i64,
                0x83 => *bytes.get(cursor).ok_or(ExecError::TruncatedInstruction)? as i8 as i64,
                0x81 => match size {
                    2 => {
                        let lo = *bytes.get(cursor).ok_or(ExecError::TruncatedInstruction)?;
                        let hi = *bytes
                            .get(cursor + 1)
                            .ok_or(ExecError::TruncatedInstruction)?;
                        i16::from_le_bytes([lo, hi]) as i64
                    }
                    4 => {
                        let mut buf = [0u8; 4];
                        buf.copy_from_slice(
                            bytes
                                .get(cursor..cursor + 4)
                                .ok_or(ExecError::TruncatedInstruction)?,
                        );
                        i32::from_le_bytes(buf) as i64
                    }
                    8 => {
                        let mut buf = [0u8; 4];
                        buf.copy_from_slice(
                            bytes
                                .get(cursor..cursor + 4)
                                .ok_or(ExecError::TruncatedInstruction)?,
                        );
                        i32::from_le_bytes(buf) as i64
                    }
                    _ => return Err(ExecError::InvalidOpcode(opcode0)),
                },
                _ => return Err(ExecError::InvalidOpcode(opcode0)),
            };
            cursor += match opcode0 {
                0x80 | 0x83 => 1,
                0x81 => match size {
                    2 => 2,
                    4 | 8 => 4,
                    _ => return Err(ExecError::InvalidOpcode(opcode0)),
                },
                _ => 0,
            };
            (AtomicKind::AluRmImm { op, imm }, size, modrm, sib, disp)
        }
        0xFE | 0xFF => {
            let (modrm, sib, disp, used) = decode_modrm(bytes, cursor, &prefixes, mode)?;
            cursor += used;
            let op = match modrm.reg {
                0 => UnaryOp::Inc,
                1 => UnaryOp::Dec,
                _ => return Err(ExecError::InvalidOpcode(opcode0)),
            };
            let size = if opcode0 == 0xFE {
                1
            } else {
                operand_size_bytes(mode, &prefixes)
            };
            (AtomicKind::Unary(op), size, modrm, sib, disp)
        }
        0xF6 | 0xF7 => {
            let (modrm, sib, disp, used) = decode_modrm(bytes, cursor, &prefixes, mode)?;
            cursor += used;
            let op = match modrm.reg {
                2 => UnaryOp::Not,
                3 => UnaryOp::Neg,
                _ => return Err(ExecError::InvalidOpcode(opcode0)),
            };
            let size = if opcode0 == 0xF6 {
                1
            } else {
                operand_size_bytes(mode, &prefixes)
            };
            (AtomicKind::Unary(op), size, modrm, sib, disp)
        }
        _ => return Err(ExecError::InvalidOpcode(opcode0)),
    };

    let inst = DecodedAtomicInst {
        len: cursor,
        prefixes,
        kind,
        size,
        modrm,
        sib,
        disp,
    };

    Ok(DecodedInst {
        len: cursor,
        kind: InstKind::Atomics(inst),
    })
}

pub fn exec_atomics<B: Bus>(
    cpu: &mut Cpu,
    bus: &mut B,
    inst: &DecodedAtomicInst,
) -> Result<(), ExecError> {
    let rex_present = inst.prefixes.rex.present;
    match &inst.kind {
        AtomicKind::CmpXchg => exec_cmpxchg(cpu, bus, inst, rex_present),
        AtomicKind::CmpXchg8B => exec_cmpxchg8b(cpu, bus, inst),
        AtomicKind::CmpXchg16B => exec_cmpxchg16b(cpu, bus, inst),
        AtomicKind::Xadd => exec_xadd(cpu, bus, inst, rex_present),
        AtomicKind::Xchg => exec_xchg(cpu, bus, inst, rex_present),
        AtomicKind::AluRmReg(op) => exec_alu_rm_reg(cpu, bus, inst, *op, rex_present),
        AtomicKind::AluRmImm { op, imm } => exec_alu_rm_imm(cpu, bus, inst, *op, *imm, rex_present),
        AtomicKind::Unary(op) => exec_unary(cpu, bus, inst, *op, rex_present),
        AtomicKind::BitOp { op, index } => exec_bit(cpu, bus, inst, *op, *index, rex_present),
    }
}

fn exec_cmpxchg<B: Bus>(
    cpu: &mut Cpu,
    bus: &mut B,
    inst: &DecodedAtomicInst,
    rex_present: bool,
) -> Result<(), ExecError> {
    let size = inst.size;
    let expected = cpu.regs.get(0, size, rex_present);
    let src = cpu.regs.get(inst.modrm.reg, size, rex_present);

    if inst.modrm.mod_bits == 0b11 {
        if inst.prefixes.lock {
            return Err(ExecError::InvalidOpcode(0));
        }
        let dst = cpu.regs.get(inst.modrm.rm, size, rex_present);
        alu::update_sub_flags(&mut cpu.rflags, expected, dst, size);
        if dst == expected {
            cpu.regs
                .set(inst.modrm.rm, size, rex_present, src, cpu.mode);
        } else {
            cpu.regs.set(0, size, rex_present, dst, cpu.mode);
        }
        return Ok(());
    }

    let addr = effective_address(cpu, inst)?;
    if inst.prefixes.lock {
        let (old, swapped) = atomic_rmw_sized(cpu, bus, addr, size, |old| {
            if old == expected {
                (src, (old, true))
            } else {
                (old, (old, false))
            }
        })?;
        alu::update_sub_flags(&mut cpu.rflags, expected, old, size);
        if !swapped {
            cpu.regs.set(0, size, rex_present, old, cpu.mode);
        }
    } else {
        let old = read_mem_sized(cpu, bus, addr, size);
        alu::update_sub_flags(&mut cpu.rflags, expected, old, size);
        if old == expected {
            write_mem_sized(cpu, bus, addr, size, src);
        } else {
            cpu.regs.set(0, size, rex_present, old, cpu.mode);
        }
    }
    Ok(())
}

fn exec_cmpxchg8b<B: Bus>(
    cpu: &mut Cpu,
    bus: &mut B,
    inst: &DecodedAtomicInst,
) -> Result<(), ExecError> {
    if inst.modrm.mod_bits == 0b11 {
        return Err(ExecError::InvalidOpcode(0));
    }
    let addr = effective_address(cpu, inst)?;
    let expected = ((cpu.regs.get(2, 4, false) as u64) << 32) | cpu.regs.get(0, 4, false) as u64;
    let replacement = ((cpu.regs.get(1, 4, false) as u64) << 32) | cpu.regs.get(3, 4, false) as u64;

    let (old, swapped) = if inst.prefixes.lock {
        atomic_rmw_sized(cpu, bus, addr, 8, |old| {
            if old == expected {
                (replacement, (old, true))
            } else {
                (old, (old, false))
            }
        })?
    } else {
        let old = read_mem_sized(cpu, bus, addr, 8);
        if old == expected {
            write_mem_sized(cpu, bus, addr, 8, replacement);
            (old, true)
        } else {
            (old, false)
        }
    };

    cpu.rflags.set_zf(swapped);
    if !swapped {
        cpu.regs.set(0, 4, false, old as u32 as u64, cpu.mode);
        cpu.regs
            .set(2, 4, false, ((old >> 32) as u32) as u64, cpu.mode);
    }
    Ok(())
}

fn exec_cmpxchg16b<B: Bus>(
    cpu: &mut Cpu,
    bus: &mut B,
    inst: &DecodedAtomicInst,
) -> Result<(), ExecError> {
    if inst.modrm.mod_bits == 0b11 {
        return Err(ExecError::InvalidOpcode(0));
    }
    let addr = effective_address(cpu, inst)?;
    if addr & 0xF != 0 {
        return Err(ExecError::Exception(Exception::gp0()));
    }
    let expected = ((cpu.regs.rdx as u128) << 64) | cpu.regs.rax as u128;
    let replacement = ((cpu.regs.rcx as u128) << 64) | cpu.regs.rbx as u128;

    let (old, swapped) = if inst.prefixes.lock {
        cpu.begin_atomic();
        let ret = bus.atomic_rmw::<u128, _>(addr, |old| {
            if old == expected {
                (replacement, (old, true))
            } else {
                (old, (old, false))
            }
        });
        cpu.log_event("atomic_rmw");
        cpu.end_atomic();
        ret.map_err(ExecError::Exception)?
    } else {
        let old = bus.read_u128(addr);
        if old == expected {
            bus.write_u128(addr, replacement);
            (old, true)
        } else {
            (old, false)
        }
    };

    cpu.rflags.set_zf(swapped);
    if !swapped {
        cpu.regs.rax = old as u64;
        cpu.regs.rdx = (old >> 64) as u64;
    }
    Ok(())
}

fn exec_xadd<B: Bus>(
    cpu: &mut Cpu,
    bus: &mut B,
    inst: &DecodedAtomicInst,
    rex_present: bool,
) -> Result<(), ExecError> {
    let size = inst.size;
    let src = cpu.regs.get(inst.modrm.reg, size, rex_present);

    if inst.modrm.mod_bits == 0b11 {
        if inst.prefixes.lock {
            return Err(ExecError::InvalidOpcode(0));
        }
        let dst = cpu.regs.get(inst.modrm.rm, size, rex_present);
        let res = alu::add_with_flags(&mut cpu.rflags, dst, src, false, size);
        cpu.regs
            .set(inst.modrm.rm, size, rex_present, res, cpu.mode);
        cpu.regs
            .set(inst.modrm.reg, size, rex_present, dst, cpu.mode);
        return Ok(());
    }

    let addr = effective_address(cpu, inst)?;
    if inst.prefixes.lock {
        let old = atomic_rmw_sized(cpu, bus, addr, size, |old| {
            let mask = mask_for_size(size);
            let new = old.wrapping_add(src) & mask;
            (new, old)
        })?;
        let _ = alu::add_with_flags(&mut cpu.rflags, old, src, false, size);
        cpu.regs
            .set(inst.modrm.reg, size, rex_present, old, cpu.mode);
    } else {
        let old = read_mem_sized(cpu, bus, addr, size);
        let res = alu::add_with_flags(&mut cpu.rflags, old, src, false, size);
        write_mem_sized(cpu, bus, addr, size, res);
        cpu.regs
            .set(inst.modrm.reg, size, rex_present, old, cpu.mode);
    }
    Ok(())
}

fn exec_xchg<B: Bus>(
    cpu: &mut Cpu,
    bus: &mut B,
    inst: &DecodedAtomicInst,
    rex_present: bool,
) -> Result<(), ExecError> {
    let size = inst.size;
    let src = cpu.regs.get(inst.modrm.reg, size, rex_present);

    if inst.modrm.mod_bits == 0b11 {
        if inst.prefixes.lock {
            return Err(ExecError::InvalidOpcode(0));
        }
        let dst = cpu.regs.get(inst.modrm.rm, size, rex_present);
        cpu.regs
            .set(inst.modrm.rm, size, rex_present, src, cpu.mode);
        cpu.regs
            .set(inst.modrm.reg, size, rex_present, dst, cpu.mode);
        return Ok(());
    }

    let addr = effective_address(cpu, inst)?;
    let old = atomic_rmw_sized(cpu, bus, addr, size, |old| (src, old))?;
    cpu.regs
        .set(inst.modrm.reg, size, rex_present, old, cpu.mode);
    Ok(())
}

fn exec_alu_rm_reg<B: Bus>(
    cpu: &mut Cpu,
    bus: &mut B,
    inst: &DecodedAtomicInst,
    op: AluOp,
    rex_present: bool,
) -> Result<(), ExecError> {
    let rhs = cpu.regs.get(inst.modrm.reg, inst.size, rex_present);
    exec_alu_rm(cpu, bus, inst, op, rhs, rex_present)
}

fn exec_alu_rm_imm<B: Bus>(
    cpu: &mut Cpu,
    bus: &mut B,
    inst: &DecodedAtomicInst,
    op: AluOp,
    imm: i64,
    rex_present: bool,
) -> Result<(), ExecError> {
    let rhs = imm as u64;
    exec_alu_rm(cpu, bus, inst, op, rhs, rex_present)
}

fn exec_alu_rm<B: Bus>(
    cpu: &mut Cpu,
    bus: &mut B,
    inst: &DecodedAtomicInst,
    op: AluOp,
    rhs_raw: u64,
    rex_present: bool,
) -> Result<(), ExecError> {
    let size = inst.size;
    let mask = mask_for_size(size);
    let rhs = rhs_raw & mask;
    let cf_in = cpu.rflags.get(RFlags::CF);

    if inst.modrm.mod_bits == 0b11 {
        if inst.prefixes.lock {
            return Err(ExecError::InvalidOpcode(0));
        }
        let lhs = cpu.regs.get(inst.modrm.rm, size, rex_present);
        let res = apply_alu(cpu, op, lhs, rhs, cf_in, size);
        cpu.regs
            .set(inst.modrm.rm, size, rex_present, res, cpu.mode);
        return Ok(());
    }

    let addr = effective_address(cpu, inst)?;
    if inst.prefixes.lock {
        let old = atomic_rmw_sized(cpu, bus, addr, size, |old| {
            let res = alu_result(op, old, rhs, cf_in, size);
            (res, old)
        })?;
        let _ = apply_alu(cpu, op, old, rhs, cf_in, size);
    } else {
        let old = read_mem_sized(cpu, bus, addr, size);
        let res = apply_alu(cpu, op, old, rhs, cf_in, size);
        write_mem_sized(cpu, bus, addr, size, res);
    }
    Ok(())
}

fn exec_unary<B: Bus>(
    cpu: &mut Cpu,
    bus: &mut B,
    inst: &DecodedAtomicInst,
    op: UnaryOp,
    rex_present: bool,
) -> Result<(), ExecError> {
    let size = inst.size;
    let mask = mask_for_size(size);
    let cf_before = cpu.rflags.get(RFlags::CF);

    if inst.modrm.mod_bits == 0b11 {
        if inst.prefixes.lock {
            return Err(ExecError::InvalidOpcode(0));
        }
        let val = cpu.regs.get(inst.modrm.rm, size, rex_present);
        let res = exec_unary_val(cpu, op, val, size, mask, cf_before);
        cpu.regs
            .set(inst.modrm.rm, size, rex_present, res, cpu.mode);
        return Ok(());
    }

    let addr = effective_address(cpu, inst)?;
    if inst.prefixes.lock {
        let old = atomic_rmw_sized(cpu, bus, addr, size, |old| {
            let new = unary_result(op, old, size, mask, cf_before);
            (new, old)
        })?;
        let _ = exec_unary_val(cpu, op, old, size, mask, cf_before);
    } else {
        let old = read_mem_sized(cpu, bus, addr, size);
        let res = exec_unary_val(cpu, op, old, size, mask, cf_before);
        write_mem_sized(cpu, bus, addr, size, res);
    }
    Ok(())
}

fn exec_bit<B: Bus>(
    cpu: &mut Cpu,
    bus: &mut B,
    inst: &DecodedAtomicInst,
    op: BitOp,
    index: BitIndex,
    rex_present: bool,
) -> Result<(), ExecError> {
    let size = inst.size;
    let bits = (size * 8) as u64;
    let bit_offset = match index {
        BitIndex::Reg(r) => cpu.regs.get(r, size, rex_present),
        BitIndex::Imm(imm) => imm as u64,
    };

    if inst.modrm.mod_bits == 0b11 {
        if inst.prefixes.lock {
            return Err(ExecError::InvalidOpcode(0));
        }
        let bit = (bit_offset & (bits - 1)) as u32;
        let val = cpu.regs.get(inst.modrm.rm, size, rex_present);
        let old = (val >> bit) & 1;
        cpu.rflags.set(RFlags::CF, old == 1);
        let res = match op {
            BitOp::Bts => val | (1u64 << bit),
            BitOp::Btr => val & !(1u64 << bit),
            BitOp::Btc => val ^ (1u64 << bit),
        };
        cpu.regs
            .set(inst.modrm.rm, size, rex_present, res, cpu.mode);
        return Ok(());
    }

    let base = effective_address(cpu, inst)?;
    let elem_index = bit_offset / bits;
    let bit = (bit_offset % bits) as u32;
    let addr = base.wrapping_add(elem_index.wrapping_mul(size as u64));

    let old_bit = if inst.prefixes.lock {
        atomic_rmw_sized(cpu, bus, addr, size, |val| {
            let old = (val >> bit) & 1;
            let res = match op {
                BitOp::Bts => val | (1u64 << bit),
                BitOp::Btr => val & !(1u64 << bit),
                BitOp::Btc => val ^ (1u64 << bit),
            };
            (res, old)
        })?
    } else {
        let val = read_mem_sized(cpu, bus, addr, size);
        let old = (val >> bit) & 1;
        let res = match op {
            BitOp::Bts => val | (1u64 << bit),
            BitOp::Btr => val & !(1u64 << bit),
            BitOp::Btc => val ^ (1u64 << bit),
        };
        write_mem_sized(cpu, bus, addr, size, res);
        old
    };
    cpu.rflags.set(RFlags::CF, old_bit == 1);
    Ok(())
}

fn decode_modrm(
    bytes: &[u8],
    cursor: usize,
    prefixes: &PrefixState,
    mode: CpuMode,
) -> Result<(ModRm, Option<Sib>, i32, usize), ExecError> {
    let modrm_byte = *bytes.get(cursor).ok_or(ExecError::TruncatedInstruction)?;
    let mod_bits = (modrm_byte >> 6) & 0b11;
    let reg = ((modrm_byte >> 3) & 0b111) | if prefixes.rex.r { 0b1000 } else { 0 };
    let rm = (modrm_byte & 0b111) | if prefixes.rex.b { 0b1000 } else { 0 };
    let modrm = ModRm { mod_bits, reg, rm };
    let mut used = 1usize;
    let mut sib = None;
    let mut disp = 0i32;

    if mod_bits != 0b11 && (rm & 0b111) == 0b100 {
        let sib_byte = *bytes
            .get(cursor + used)
            .ok_or(ExecError::TruncatedInstruction)?;
        used += 1;
        let scale = (sib_byte >> 6) & 0b11;
        let index = ((sib_byte >> 3) & 0b111) | if prefixes.rex.x { 0b1000 } else { 0 };
        let base = (sib_byte & 0b111) | if prefixes.rex.b { 0b1000 } else { 0 };
        sib = Some(Sib { scale, index, base });
    }

    match mod_bits {
        0b00 => {
            let needs_disp32 = if let Some(sib) = sib {
                (sib.base & 0b111) == 0b101
            } else {
                (rm & 0b111) == 0b101
            };
            if needs_disp32 {
                let mut buf = [0u8; 4];
                buf.copy_from_slice(
                    bytes
                        .get(cursor + used..cursor + used + 4)
                        .ok_or(ExecError::TruncatedInstruction)?,
                );
                disp = i32::from_le_bytes(buf);
                used += 4;
            }
        }
        0b01 => {
            disp = *bytes
                .get(cursor + used)
                .ok_or(ExecError::TruncatedInstruction)? as i8 as i32;
            used += 1;
        }
        0b10 => {
            let mut buf = [0u8; 4];
            buf.copy_from_slice(
                bytes
                    .get(cursor + used..cursor + used + 4)
                    .ok_or(ExecError::TruncatedInstruction)?,
            );
            disp = i32::from_le_bytes(buf);
            used += 4;
        }
        _ => {}
    }

    let _ = mode;
    Ok((modrm, sib, disp, used))
}

fn operand_size_bytes(mode: CpuMode, p: &PrefixState) -> usize {
    match mode {
        CpuMode::Real16 => {
            if p.operand_size_override {
                4
            } else {
                2
            }
        }
        CpuMode::Protected32 => {
            if p.operand_size_override {
                2
            } else {
                4
            }
        }
        CpuMode::Long64 => {
            if p.rex.w {
                8
            } else if p.operand_size_override {
                2
            } else {
                4
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AddrSize {
    A16,
    A32,
    A64,
}

fn addr_size(mode: CpuMode, p: &PrefixState) -> AddrSize {
    match mode {
        CpuMode::Real16 => {
            if p.address_size_override {
                AddrSize::A32
            } else {
                AddrSize::A16
            }
        }
        CpuMode::Protected32 => {
            if p.address_size_override {
                AddrSize::A16
            } else {
                AddrSize::A32
            }
        }
        CpuMode::Long64 => {
            if p.address_size_override {
                AddrSize::A32
            } else {
                AddrSize::A64
            }
        }
    }
}

fn effective_address(cpu: &Cpu, inst: &DecodedAtomicInst) -> Result<u64, ExecError> {
    let addr_size = addr_size(cpu.mode, &inst.prefixes);
    if addr_size == AddrSize::A16 {
        return Err(ExecError::Exception(Exception::Unimplemented(
            "16-bit addressing",
        )));
    }
    let disp = inst.disp as i64;
    let disp_u64 = disp as u64;

    let mut base_reg: Option<u8> = None;
    let offset = if let Some(sib) = inst.sib {
        let scale = 1u64 << sib.scale;
        let base_is_none = inst.modrm.mod_bits == 0b00 && (sib.base & 0b111) == 0b101;
        let base = if base_is_none {
            0
        } else {
            base_reg = Some(sib.base);
            read_addr_reg(cpu, sib.base, addr_size)
        };
        let index = if (sib.index & 0b111) == 0b100 {
            0
        } else {
            read_addr_reg(cpu, sib.index, addr_size).wrapping_mul(scale)
        };
        add_addr(
            add_addr(base, index as i64, addr_size),
            disp_u64 as i64,
            addr_size,
        )
    } else {
        let rm_low = inst.modrm.rm & 0b111;
        let no_base = inst.modrm.mod_bits == 0b00 && rm_low == 0b101;
        if no_base {
            match (cpu.mode, addr_size) {
                (CpuMode::Long64, AddrSize::A64) => {
                    cpu.rip.wrapping_add(inst.len as u64).wrapping_add(disp_u64)
                }
                _ => disp_u64,
            }
        } else {
            base_reg = Some(inst.modrm.rm);
            add_addr(
                read_addr_reg(cpu, inst.modrm.rm, addr_size),
                disp_u64 as i64,
                addr_size,
            )
        }
    };

    let default_seg = match base_reg.map(|r| r & 0b111) {
        Some(4) | Some(5) => Segment::Ss,
        _ => Segment::Ds,
    };
    let seg = inst.prefixes.segment_override.unwrap_or(default_seg);
    Ok(cpu.seg_base(seg).wrapping_add(offset))
}

fn read_addr_reg(cpu: &Cpu, reg: u8, addr_size: AddrSize) -> u64 {
    let v = cpu.regs.get(reg, 8, true);
    match addr_size {
        AddrSize::A32 => v as u32 as u64,
        AddrSize::A64 => v,
        AddrSize::A16 => v as u16 as u64,
    }
}

fn add_addr(base: u64, delta: i64, addr_size: AddrSize) -> u64 {
    match addr_size {
        AddrSize::A32 => (base as u32).wrapping_add(delta as u32) as u64,
        AddrSize::A64 => base.wrapping_add(delta as u64),
        AddrSize::A16 => (base as u16).wrapping_add(delta as u16) as u64,
    }
}

fn mask_for_size(size: usize) -> u64 {
    let bits = (size * 8) as u32;
    if bits == 64 {
        u64::MAX
    } else {
        (1u64 << bits) - 1
    }
}

fn read_mem_sized<B: Bus>(cpu: &mut Cpu, bus: &mut B, addr: u64, size: usize) -> u64 {
    let v = match size {
        1 => bus.read_u8(addr) as u64,
        2 => bus.read_u16(addr) as u64,
        4 => bus.read_u32(addr) as u64,
        8 => bus.read_u64(addr),
        _ => bus.read_u64(addr),
    };
    cpu.maybe_deliver_interrupts();
    v
}

fn write_mem_sized<B: Bus>(cpu: &mut Cpu, bus: &mut B, addr: u64, size: usize, value: u64) {
    match size {
        1 => bus.write_u8(addr, value as u8),
        2 => bus.write_u16(addr, value as u16),
        4 => bus.write_u32(addr, value as u32),
        8 => bus.write_u64(addr, value),
        _ => bus.write_u64(addr, value),
    }
    cpu.maybe_deliver_interrupts();
}

fn atomic_rmw_sized<B: Bus, R>(
    cpu: &mut Cpu,
    bus: &mut B,
    addr: u64,
    size: usize,
    f: impl FnOnce(u64) -> (u64, R),
) -> Result<R, ExecError> {
    cpu.begin_atomic();
    let ret = match size {
        1 => bus.atomic_rmw::<u8, _>(addr, |old| {
            let (new, r) = f(old as u64);
            (new as u8, r)
        }),
        2 => bus.atomic_rmw::<u16, _>(addr, |old| {
            let (new, r) = f(old as u64);
            (new as u16, r)
        }),
        4 => bus.atomic_rmw::<u32, _>(addr, |old| {
            let (new, r) = f(old as u64);
            (new as u32, r)
        }),
        8 => bus.atomic_rmw::<u64, _>(addr, |old| f(old)),
        _ => bus.atomic_rmw::<u64, _>(addr, |old| f(old)),
    };
    cpu.log_event("atomic_rmw");
    cpu.end_atomic();
    ret.map_err(ExecError::Exception)
}

fn alu_result(op: AluOp, lhs: u64, rhs: u64, cf_in: bool, size: usize) -> u64 {
    let mask = mask_for_size(size);
    match op {
        AluOp::Add => lhs.wrapping_add(rhs) & mask,
        AluOp::Adc => lhs.wrapping_add(rhs).wrapping_add(cf_in as u64) & mask,
        AluOp::Sub => lhs.wrapping_sub(rhs) & mask,
        AluOp::Sbb => lhs.wrapping_sub(rhs).wrapping_sub(cf_in as u64) & mask,
        AluOp::And => (lhs & rhs) & mask,
        AluOp::Or => (lhs | rhs) & mask,
        AluOp::Xor => (lhs ^ rhs) & mask,
    }
}

fn apply_alu(cpu: &mut Cpu, op: AluOp, lhs: u64, rhs: u64, cf_in: bool, size: usize) -> u64 {
    match op {
        AluOp::Add => alu::add_with_flags(&mut cpu.rflags, lhs, rhs, false, size),
        AluOp::Adc => alu::add_with_flags(&mut cpu.rflags, lhs, rhs, cf_in, size),
        AluOp::Sub => alu::sub_with_flags(&mut cpu.rflags, lhs, rhs, false, size),
        AluOp::Sbb => alu::sub_with_flags(&mut cpu.rflags, lhs, rhs, cf_in, size),
        AluOp::And => alu::logic_with_flags(&mut cpu.rflags, lhs & rhs, size),
        AluOp::Or => alu::logic_with_flags(&mut cpu.rflags, lhs | rhs, size),
        AluOp::Xor => alu::logic_with_flags(&mut cpu.rflags, lhs ^ rhs, size),
    }
}

fn unary_result(op: UnaryOp, value: u64, _size: usize, mask: u64, _cf_before: bool) -> u64 {
    match op {
        UnaryOp::Inc => value.wrapping_add(1) & mask,
        UnaryOp::Dec => value.wrapping_sub(1) & mask,
        UnaryOp::Neg => (0u64.wrapping_sub(value)) & mask,
        UnaryOp::Not => (!value) & mask,
    }
}

fn exec_unary_val(
    cpu: &mut Cpu,
    op: UnaryOp,
    value: u64,
    size: usize,
    mask: u64,
    cf_before: bool,
) -> u64 {
    match op {
        UnaryOp::Inc => {
            let res = alu::add_with_flags(&mut cpu.rflags, value, 1, false, size);
            cpu.rflags.set(RFlags::CF, cf_before);
            res
        }
        UnaryOp::Dec => {
            let res = alu::sub_with_flags(&mut cpu.rflags, value, 1, false, size);
            cpu.rflags.set(RFlags::CF, cf_before);
            res
        }
        UnaryOp::Neg => alu::sub_with_flags(&mut cpu.rflags, 0, value, false, size),
        UnaryOp::Not => (!value) & mask,
    }
}
