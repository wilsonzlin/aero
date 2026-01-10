use crate::cpu::{CpuState, Reg};
use crate::interpreter::{Exception, Next};

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpcodeKind {
    Invalid = 0,
    Nop = 1,
    Hlt = 2,
    DecReg = 3,
    JnzRel = 4,
    RepMovsb = 5,
    Sti = 6,
    MovSsAx = 7,
}

impl OpcodeKind {
    pub const COUNT: usize = 8;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodedInst {
    pub opcode: OpcodeKind,
    pub len: u8,
    pub reg: u8,
    pub disp: i32,
}

impl DecodedInst {
    pub fn new_simple(opcode: OpcodeKind, len: u8) -> Self {
        Self {
            opcode,
            len,
            reg: 0,
            disp: 0,
        }
    }

    pub fn new_reg(opcode: OpcodeKind, len: u8, reg: u8) -> Self {
        Self {
            opcode,
            len,
            reg,
            disp: 0,
        }
    }

    pub fn new_jcc(opcode: OpcodeKind, len: u8, disp: i32) -> Self {
        Self {
            opcode,
            len,
            reg: 0,
            disp,
        }
    }
}

pub type Handler<B> = fn(&mut CpuState, &mut B, &DecodedInst) -> Result<Next, Exception>;

#[inline(always)]
pub fn op_invalid<B>(
    _cpu: &mut CpuState,
    _bus: &mut B,
    _inst: &DecodedInst,
) -> Result<Next, Exception> {
    Err(Exception::InvalidOpcode)
}

#[inline(always)]
pub fn op_nop<B>(
    _cpu: &mut CpuState,
    _bus: &mut B,
    _inst: &DecodedInst,
) -> Result<Next, Exception> {
    Ok(Next::Continue)
}

#[inline(always)]
pub fn op_hlt<B>(
    _cpu: &mut CpuState,
    _bus: &mut B,
    _inst: &DecodedInst,
) -> Result<Next, Exception> {
    Ok(Next::Exit)
}

#[inline(always)]
pub fn op_sti<B>(cpu: &mut CpuState, _bus: &mut B, _inst: &DecodedInst) -> Result<Next, Exception> {
    cpu.flags.iflag = true;
    cpu.interrupt_shadow = 2;
    Ok(Next::Continue)
}

#[inline(always)]
pub fn op_mov_ss_ax<B>(
    cpu: &mut CpuState,
    _bus: &mut B,
    _inst: &DecodedInst,
) -> Result<Next, Exception> {
    cpu.ss = (cpu.reg(Reg::Rax) & 0xFFFF) as u16;
    cpu.interrupt_shadow = 2;
    Ok(Next::Continue)
}

#[inline(always)]
pub fn op_dec_reg<B>(
    cpu: &mut CpuState,
    _bus: &mut B,
    inst: &DecodedInst,
) -> Result<Next, Exception> {
    let value = cpu.reg_by_index(inst.reg).wrapping_sub(1);
    cpu.set_reg_by_index(inst.reg, value);
    cpu.flags.zf = value == 0;
    Ok(Next::Continue)
}

#[inline(always)]
pub fn op_jnz_rel<B>(
    cpu: &mut CpuState,
    _bus: &mut B,
    inst: &DecodedInst,
) -> Result<Next, Exception> {
    if !cpu.flags.zf {
        let next_rip = cpu.rip.wrapping_add(inst.len as u64);
        let target = next_rip.wrapping_add(inst.disp as i64 as u64);
        Ok(Next::Jump(target))
    } else {
        Ok(Next::Continue)
    }
}

#[inline(always)]
fn rep_movsb_slow<B: crate::bus::CpuBus>(
    cpu: &mut CpuState,
    bus: &mut B,
) -> Result<Next, Exception> {
    // REP MOVSB: copy RCX bytes from [RSI] to [RDI], updating RSI/RDI and
    // decrementing RCX each iteration.
    //
    // Interruptibility: x86 allows interrupts between iterations, but STI/MOV SS
    // shadowing blocks them until after the instruction completes. We therefore
    // only perform mid-iteration interrupt checks when no shadow is active.
    let mut count = cpu.reg(Reg::Rcx);
    if count == 0 {
        return Ok(Next::Continue);
    }

    let df = cpu.flags.df;
    let step: i64 = if df { -1 } else { 1 };

    let mut src = cpu.reg(Reg::Rsi) as i64;
    let mut dst = cpu.reg(Reg::Rdi) as i64;

    while count != 0 {
        let byte = bus.read_u8(src as u64)?;
        bus.write_u8(dst as u64, byte)?;
        src += step;
        dst += step;
        count -= 1;
        cpu.set_reg(Reg::Rcx, count);

        // Optional mid-iteration interrupt check.
        if cpu.interrupt_shadow == 0 && cpu.flags.iflag && cpu.pending_interrupt.is_some() {
            // Leave RIP pointing at the REP MOVSB instruction to resume.
            cpu.set_reg(Reg::Rsi, src as u64);
            cpu.set_reg(Reg::Rdi, dst as u64);
            return Err(Exception::Interrupt(cpu.pending_interrupt.take().unwrap()));
        }
    }

    cpu.set_reg(Reg::Rsi, src as u64);
    cpu.set_reg(Reg::Rdi, dst as u64);
    Ok(Next::Continue)
}

#[inline(always)]
pub fn op_rep_movsb_slow<B: crate::bus::CpuBus>(
    cpu: &mut CpuState,
    bus: &mut B,
    _inst: &DecodedInst,
) -> Result<Next, Exception> {
    rep_movsb_slow(cpu, bus)
}

#[inline(always)]
pub fn op_rep_movsb_fast<B: crate::bus::CpuBus>(
    cpu: &mut CpuState,
    bus: &mut B,
    _inst: &DecodedInst,
) -> Result<Next, Exception> {
    let count = cpu.reg(Reg::Rcx);
    if count == 0 {
        return Ok(Next::Continue);
    }

    // If the REP instruction is interruptible in our model (IF=1, no STI/MOV SS
    // shadow, and an interrupt is already pending), fall back to the slow path
    // which checks between iterations.
    if cpu.interrupt_shadow == 0 && cpu.flags.iflag && cpu.pending_interrupt.is_some() {
        return rep_movsb_slow(cpu, bus);
    }

    let df = cpu.flags.df;
    if df {
        return rep_movsb_slow(cpu, bus);
    }

    // Bulk-copy fast path (DF=0, non-overlapping ranges on contiguous memory).
    if let Some(mem_bus) = bus.as_any_mut().downcast_mut::<crate::bus::MemoryBus>() {
        let len = count as usize;
        let src_u = cpu.reg(Reg::Rsi);
        let dst_u = cpu.reg(Reg::Rdi);
        let src_end = src_u.wrapping_add(len as u64);
        let dst_end = dst_u.wrapping_add(len as u64);
        let non_overlapping = src_end <= dst_u || dst_end <= src_u;

        if non_overlapping {
            let mem = mem_bus.as_mut_slice();
            let src_range = src_u as usize..src_end as usize;
            mem.copy_within(src_range, dst_u as usize);
            mem_bus.bump_versions_for_write(dst_u, len);

            cpu.set_reg(Reg::Rsi, src_end);
            cpu.set_reg(Reg::Rdi, dst_end);
            cpu.set_reg(Reg::Rcx, 0);
            return Ok(Next::Continue);
        }
    }

    rep_movsb_slow(cpu, bus)
}
