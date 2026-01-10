use crate::cpu::CpuState;
use crate::ir::{BinOp, CmpOp, IrBlock, IrOp, MemSize, Operand, Place, Temp};

pub const JIT_EXIT_SENTINEL: u64 = u64::MAX;

pub fn interpret_block(block: &IrBlock, cpu: &mut CpuState, mem: &mut [u8]) -> u64 {
    let mut temps = vec![0i64; block.temp_count as usize];
    let mut next_rip: Option<u64> = None;

    for op in &block.ops {
        match *op {
            IrOp::Set { dst, src } => {
                let v = eval_operand(src, cpu, &temps);
                write_place(dst, v, cpu, &mut temps);
            }
            IrOp::Bin { dst, op, lhs, rhs } => {
                let a = eval_operand(lhs, cpu, &temps);
                let b = eval_operand(rhs, cpu, &temps);
                let r = match op {
                    BinOp::Add => a.wrapping_add(b),
                    BinOp::Sub => a.wrapping_sub(b),
                    BinOp::And => a & b,
                    BinOp::Or => a | b,
                    BinOp::Xor => a ^ b,
                    BinOp::Shl => a.wrapping_shl((b as u64 & 63) as u32),
                    BinOp::ShrU => ((a as u64).wrapping_shr((b as u64 & 63) as u32)) as i64,
                };
                write_place(dst, r, cpu, &mut temps);
            }
            IrOp::Cmp { dst, op, lhs, rhs } => {
                let a = eval_operand(lhs, cpu, &temps);
                let b = eval_operand(rhs, cpu, &temps);
                let r = match op {
                    CmpOp::Eq => a == b,
                    CmpOp::Ne => a != b,
                    CmpOp::LtS => a < b,
                    CmpOp::LtU => (a as u64) < (b as u64),
                    CmpOp::LeS => a <= b,
                    CmpOp::LeU => (a as u64) <= (b as u64),
                    CmpOp::GtS => a > b,
                    CmpOp::GtU => (a as u64) > (b as u64),
                    CmpOp::GeS => a >= b,
                    CmpOp::GeU => (a as u64) >= (b as u64),
                };
                write_place(dst, r as i64, cpu, &mut temps);
            }
            IrOp::Select {
                dst,
                cond,
                if_true,
                if_false,
            } => {
                let c = eval_operand(cond, cpu, &temps);
                let t = eval_operand(if_true, cpu, &temps);
                let f = eval_operand(if_false, cpu, &temps);
                write_place(dst, if c != 0 { t } else { f }, cpu, &mut temps);
            }
            IrOp::Load { dst, addr, size } => {
                let addr = eval_operand(addr, cpu, &temps) as u64;
                let v = load_mem(mem, addr, size);
                write_place(dst, v as i64, cpu, &mut temps);
            }
            IrOp::Store { addr, value, size } => {
                let addr = eval_operand(addr, cpu, &temps) as u64;
                let v = eval_operand(value, cpu, &temps) as u64;
                store_mem(mem, addr, v, size);
            }
            IrOp::Exit { next_rip: rip } => {
                let rip = eval_operand(rip, cpu, &temps) as u64;
                next_rip = Some(rip);
                break;
            }
            IrOp::ExitIf {
                cond,
                next_rip: rip,
            } => {
                let c = eval_operand(cond, cpu, &temps);
                if c != 0 {
                    let rip = eval_operand(rip, cpu, &temps) as u64;
                    next_rip = Some(rip);
                    break;
                }
            }
            IrOp::Bailout { rip, .. } => {
                let _rip = eval_operand(rip, cpu, &temps) as u64;
                next_rip = Some(JIT_EXIT_SENTINEL);
                break;
            }
        }
    }

    let next_rip = next_rip.expect("IR block did not terminate with Exit/Bailout");
    cpu.rip = next_rip;
    next_rip
}

fn eval_operand(op: Operand, cpu: &CpuState, temps: &[i64]) -> i64 {
    match op {
        Operand::Imm(v) => v,
        Operand::Reg(r) => cpu.get_reg(r) as i64,
        Operand::Temp(Temp(t)) => temps[t as usize],
    }
}

fn write_place(place: Place, val: i64, cpu: &mut CpuState, temps: &mut [i64]) {
    match place {
        Place::Reg(r) => cpu.set_reg(r, val as u64),
        Place::Temp(Temp(t)) => temps[t as usize] = val,
    }
}

fn load_mem(mem: &[u8], addr: u64, size: MemSize) -> u64 {
    let addr = addr as usize;
    match size {
        MemSize::U8 => mem[addr] as u64,
        MemSize::U16 => u16::from_le_bytes(mem[addr..addr + 2].try_into().unwrap()) as u64,
        MemSize::U32 => u32::from_le_bytes(mem[addr..addr + 4].try_into().unwrap()) as u64,
        MemSize::U64 => u64::from_le_bytes(mem[addr..addr + 8].try_into().unwrap()),
    }
}

fn store_mem(mem: &mut [u8], addr: u64, value: u64, size: MemSize) {
    let addr = addr as usize;
    match size {
        MemSize::U8 => mem[addr] = value as u8,
        MemSize::U16 => mem[addr..addr + 2].copy_from_slice(&(value as u16).to_le_bytes()),
        MemSize::U32 => mem[addr..addr + 4].copy_from_slice(&(value as u32).to_le_bytes()),
        MemSize::U64 => mem[addr..addr + 8].copy_from_slice(&value.to_le_bytes()),
    }
}
