use crate::exception::Exception;
use crate::interp::x87::{Fault as X87Fault, X87};
use crate::mem::CpuBus;
use crate::state::{
    CpuState, CR0_EM, CR0_MP, CR0_NE, CR0_TS, FLAG_AF, FLAG_CF, FLAG_OF, FLAG_PF, FLAG_SF, FLAG_ZF,
};
use aero_x86::{DecodedInst, Instruction, MemorySize, Mnemonic, OpKind, Register};

use super::ops_data::calc_ea;
use super::ExecOutcome;

pub fn handles_mnemonic(m: Mnemonic) -> bool {
    matches!(
        m,
        Mnemonic::Finit
            | Mnemonic::Fninit
            | Mnemonic::Fclex
            | Mnemonic::Fnclex
            | Mnemonic::Ffree
            | Mnemonic::Ffreep
            | Mnemonic::Fxch
            | Mnemonic::Fld1
            | Mnemonic::Fldz
            | Mnemonic::Fincstp
            | Mnemonic::Fdecstp
            | Mnemonic::Fld
            | Mnemonic::Fst
            | Mnemonic::Fstp
            | Mnemonic::Fild
            | Mnemonic::Fistp
            | Mnemonic::Fadd
            | Mnemonic::Faddp
            | Mnemonic::Fsub
            | Mnemonic::Fsubp
            | Mnemonic::Fsubr
            | Mnemonic::Fsubrp
            | Mnemonic::Fmul
            | Mnemonic::Fmulp
            | Mnemonic::Fdiv
            | Mnemonic::Fdivp
            | Mnemonic::Fdivr
            | Mnemonic::Fdivrp
            | Mnemonic::Fchs
            | Mnemonic::Fabs
            | Mnemonic::Fcom
            | Mnemonic::Fcomp
            | Mnemonic::Fcompp
            | Mnemonic::Fcomi
            | Mnemonic::Fcomip
            | Mnemonic::Fucomi
            | Mnemonic::Fucomip
            | Mnemonic::Fldcw
            | Mnemonic::Fstcw
            | Mnemonic::Fnstcw
            | Mnemonic::Fnstsw
            | Mnemonic::Fstsw
            | Mnemonic::Wait
    )
}

pub fn exec<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    decoded: &DecodedInst,
    next_ip: u64,
) -> Result<ExecOutcome, Exception> {
    let instr = &decoded.instr;
    if instr.mnemonic() == Mnemonic::Wait {
        return exec_wait(state);
    }

    check_x87_available(state)?;

    // The architectural x87 state is stored in `state.fpu` (FXSAVE-compatible
    // image). For convenience, the x87 interpreter operates on a transient
    // `X87` value and writes the result back after every instruction.
    let mut x87 = X87::default();
    x87.load_from_fpu_state(&state.fpu);

    let res = (|| {
        match instr.mnemonic() {
            Mnemonic::Finit | Mnemonic::Fninit => {
                x87.fninit();
                Ok(ExecOutcome::Continue)
            }
            Mnemonic::Fclex | Mnemonic::Fnclex => {
                x87.fnclex();
                Ok(ExecOutcome::Continue)
            }
            Mnemonic::Fincstp => {
                x87.fincstp();
                Ok(ExecOutcome::Continue)
            }
            Mnemonic::Fdecstp => {
                x87.fdecstp();
                Ok(ExecOutcome::Continue)
            }
            Mnemonic::Fld1 => {
                x87.fld1().map_err(map_x87_fault)?;
                Ok(ExecOutcome::Continue)
            }
            Mnemonic::Fldz => {
                x87.fldz().map_err(map_x87_fault)?;
                Ok(ExecOutcome::Continue)
            }
            Mnemonic::Fxch => {
                exec_fxch(&mut x87, instr)?;
                Ok(ExecOutcome::Continue)
            }
            Mnemonic::Ffree | Mnemonic::Ffreep => {
                exec_ffree(&mut x87, instr)?;
                Ok(ExecOutcome::Continue)
            }
            Mnemonic::Fld => {
                exec_fld(state, &mut x87, bus, instr, next_ip)?;
                Ok(ExecOutcome::Continue)
            }
            Mnemonic::Fst => {
                exec_fst(state, &mut x87, bus, instr, next_ip, false)?;
                Ok(ExecOutcome::Continue)
            }
            Mnemonic::Fstp => {
                exec_fst(state, &mut x87, bus, instr, next_ip, true)?;
                Ok(ExecOutcome::Continue)
            }
            Mnemonic::Fild => {
                exec_fild(state, &mut x87, bus, instr, next_ip)?;
                Ok(ExecOutcome::Continue)
            }
            Mnemonic::Fistp => {
                exec_fistp(state, &mut x87, bus, instr, next_ip)?;
                Ok(ExecOutcome::Continue)
            }
            Mnemonic::Fadd
            | Mnemonic::Fsub
            | Mnemonic::Fsubr
            | Mnemonic::Fmul
            | Mnemonic::Fdiv
            | Mnemonic::Fdivr => {
                exec_binop(state, &mut x87, bus, instr, next_ip)?;
                Ok(ExecOutcome::Continue)
            }
            Mnemonic::Faddp
            | Mnemonic::Fsubp
            | Mnemonic::Fsubrp
            | Mnemonic::Fmulp
            | Mnemonic::Fdivp
            | Mnemonic::Fdivrp => {
                exec_binop_pop(&mut x87, instr)?;
                Ok(ExecOutcome::Continue)
            }
            Mnemonic::Fchs => {
                x87.fchs().map_err(map_x87_fault)?;
                Ok(ExecOutcome::Continue)
            }
            Mnemonic::Fabs => {
                x87.fabs().map_err(map_x87_fault)?;
                Ok(ExecOutcome::Continue)
            }
            Mnemonic::Fcom | Mnemonic::Fcomp => {
                exec_compare(state, &mut x87, bus, instr, next_ip)?;
                Ok(ExecOutcome::Continue)
            }
            Mnemonic::Fcompp => {
                x87.fcompp().map_err(map_x87_fault)?;
                Ok(ExecOutcome::Continue)
            }
            Mnemonic::Fcomi | Mnemonic::Fcomip | Mnemonic::Fucomi | Mnemonic::Fucomip => {
                exec_comparei(state, &mut x87, instr)?;
                Ok(ExecOutcome::Continue)
            }
            Mnemonic::Fldcw => {
                let addr = memory_addr(state, instr, next_ip)?;
                let cw = bus.read_u16(addr)?;
                x87.fldcw(cw);
                Ok(ExecOutcome::Continue)
            }
            Mnemonic::Fnstcw | Mnemonic::Fstcw => {
                let addr = memory_addr(state, instr, next_ip)?;
                let cw = x87.fnstcw();
                bus.write_u16(addr, cw)?;
                Ok(ExecOutcome::Continue)
            }
            Mnemonic::Fnstsw | Mnemonic::Fstsw => {
                exec_fnstsw(state, &mut x87, bus, instr, next_ip)?;
                Ok(ExecOutcome::Continue)
            }
            _ => Err(Exception::InvalidOpcode),
        }
    })();

    x87.store_to_fpu_state(&mut state.fpu);
    res
}

fn check_x87_available(state: &CpuState) -> Result<(), Exception> {
    let cr0 = state.control.cr0;
    if (cr0 & CR0_EM) != 0 {
        return Err(Exception::InvalidOpcode);
    }
    if (cr0 & CR0_TS) != 0 {
        return Err(Exception::DeviceNotAvailable);
    }
    Ok(())
}

fn exec_wait(state: &mut CpuState) -> Result<ExecOutcome, Exception> {
    let cr0 = state.control.cr0;
    if (cr0 & CR0_EM) != 0 {
        return Err(Exception::InvalidOpcode);
    }
    if (cr0 & CR0_MP) != 0 && (cr0 & CR0_TS) != 0 {
        return Err(Exception::DeviceNotAvailable);
    }

    if state.fpu.has_unmasked_exception() {
        if (cr0 & CR0_NE) != 0 {
            return Err(Exception::X87Fpu);
        }
        state.irq13_pending = true;
    }

    Ok(ExecOutcome::Continue)
}

fn map_x87_fault(_: X87Fault) -> Exception {
    Exception::X87Fpu
}

fn st_index(reg: Register) -> Option<usize> {
    Some(match reg {
        Register::ST0 => 0,
        Register::ST1 => 1,
        Register::ST2 => 2,
        Register::ST3 => 3,
        Register::ST4 => 4,
        Register::ST5 => 5,
        Register::ST6 => 6,
        Register::ST7 => 7,
        _ => return None,
    })
}

fn memory_addr(state: &CpuState, instr: &Instruction, next_ip: u64) -> Result<u64, Exception> {
    calc_ea(state, instr, next_ip, true)
}

fn read_mem_f32<B: CpuBus>(bus: &mut B, addr: u64) -> Result<f32, Exception> {
    Ok(f32::from_bits(bus.read_u32(addr)?))
}

fn read_mem_f64<B: CpuBus>(bus: &mut B, addr: u64) -> Result<f64, Exception> {
    Ok(f64::from_bits(bus.read_u64(addr)?))
}

fn exec_fld<B: CpuBus>(
    state: &mut CpuState,
    x87: &mut X87,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<(), Exception> {
    match instr.op_kind(0) {
        OpKind::Memory => {
            let addr = memory_addr(state, instr, next_ip)?;
            match instr.memory_size() {
                MemorySize::Float32 => {
                    let v = read_mem_f32(bus, addr)?;
                    x87.fld_f32(v).map_err(map_x87_fault)?;
                }
                MemorySize::Float64 => {
                    let v = read_mem_f64(bus, addr)?;
                    x87.fld_f64(v).map_err(map_x87_fault)?;
                }
                _ => return Err(Exception::InvalidOpcode),
            }
        }
        OpKind::Register => {
            let reg = instr.op0_register();
            let i = st_index(reg).ok_or(Exception::InvalidOpcode)?;
            x87.fld_st(i).map_err(map_x87_fault)?;
        }
        _ => return Err(Exception::InvalidOpcode),
    }
    Ok(())
}

fn exec_fst<B: CpuBus>(
    state: &mut CpuState,
    x87: &mut X87,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
    pop: bool,
) -> Result<(), Exception> {
    match instr.op_kind(0) {
        OpKind::Memory => {
            let addr = memory_addr(state, instr, next_ip)?;
            match instr.memory_size() {
                MemorySize::Float32 => {
                    let v = if pop {
                        x87.fstp_f32().map_err(map_x87_fault)?
                    } else {
                        x87.fst_f32().map_err(map_x87_fault)?
                    };
                    bus.write_u32(addr, v.to_bits())?;
                }
                MemorySize::Float64 => {
                    let v = if pop {
                        x87.fstp_f64().map_err(map_x87_fault)?
                    } else {
                        x87.fst_f64().map_err(map_x87_fault)?
                    };
                    bus.write_u64(addr, v.to_bits())?;
                }
                _ => return Err(Exception::InvalidOpcode),
            }
        }
        OpKind::Register => {
            let reg = instr.op0_register();
            let i = st_index(reg).ok_or(Exception::InvalidOpcode)?;
            if pop {
                x87.fstp_st(i).map_err(map_x87_fault)?;
            } else {
                x87.fst_st(i).map_err(map_x87_fault)?;
            }
        }
        _ => return Err(Exception::InvalidOpcode),
    }
    Ok(())
}

fn exec_fild<B: CpuBus>(
    state: &mut CpuState,
    x87: &mut X87,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<(), Exception> {
    let addr = memory_addr(state, instr, next_ip)?;
    match instr.memory_size() {
        MemorySize::Int16 => {
            let v = bus.read_u16(addr)? as i16;
            x87.fild_i16(v).map_err(map_x87_fault)?;
        }
        MemorySize::Int32 => {
            let v = bus.read_u32(addr)? as i32;
            x87.fild_i32(v).map_err(map_x87_fault)?;
        }
        MemorySize::Int64 => {
            let v = bus.read_u64(addr)? as i64;
            x87.fild_i64(v).map_err(map_x87_fault)?;
        }
        _ => return Err(Exception::InvalidOpcode),
    }
    Ok(())
}

fn exec_fistp<B: CpuBus>(
    state: &mut CpuState,
    x87: &mut X87,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<(), Exception> {
    let addr = memory_addr(state, instr, next_ip)?;
    match instr.memory_size() {
        MemorySize::Int16 => {
            let v = x87.fistp_i16().map_err(map_x87_fault)?;
            bus.write_u16(addr, v as u16)?;
        }
        MemorySize::Int32 => {
            let v = x87.fistp_i32().map_err(map_x87_fault)?;
            bus.write_u32(addr, v as u32)?;
        }
        MemorySize::Int64 => {
            let v = x87.fistp_i64().map_err(map_x87_fault)?;
            bus.write_u64(addr, v as u64)?;
        }
        _ => return Err(Exception::InvalidOpcode),
    }
    Ok(())
}

fn exec_binop<B: CpuBus>(
    state: &mut CpuState,
    x87: &mut X87,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<(), Exception> {
    match instr.op_kind(0) {
        OpKind::Memory => exec_binop_mem(state, x87, bus, instr, next_ip),
        OpKind::Register => exec_binop_reg(x87, instr),
        _ => Err(Exception::InvalidOpcode),
    }
}

fn exec_binop_mem<B: CpuBus>(
    state: &mut CpuState,
    x87: &mut X87,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<(), Exception> {
    let addr = memory_addr(state, instr, next_ip)?;
    match instr.memory_size() {
        MemorySize::Float32 => {
            let v = read_mem_f32(bus, addr)?;
            match instr.mnemonic() {
                Mnemonic::Fadd => x87.fadd_m32(v),
                Mnemonic::Fsub => x87.fsub_m32(v),
                Mnemonic::Fsubr => x87.fsubr_m32(v),
                Mnemonic::Fmul => x87.fmul_m32(v),
                Mnemonic::Fdiv => x87.fdiv_m32(v),
                Mnemonic::Fdivr => x87.fdivr_m32(v),
                _ => return Err(Exception::InvalidOpcode),
            }
            .map_err(map_x87_fault)?;
        }
        MemorySize::Float64 => {
            let v = read_mem_f64(bus, addr)?;
            match instr.mnemonic() {
                Mnemonic::Fadd => x87.fadd_m64(v),
                Mnemonic::Fsub => x87.fsub_m64(v),
                Mnemonic::Fsubr => x87.fsubr_m64(v),
                Mnemonic::Fmul => x87.fmul_m64(v),
                Mnemonic::Fdiv => x87.fdiv_m64(v),
                Mnemonic::Fdivr => x87.fdivr_m64(v),
                _ => return Err(Exception::InvalidOpcode),
            }
            .map_err(map_x87_fault)?;
        }
        _ => return Err(Exception::InvalidOpcode),
    }
    Ok(())
}

fn exec_binop_reg(x87: &mut X87, instr: &Instruction) -> Result<(), Exception> {
    let (dest, src) = x87_reg_pair(instr)?;
    match instr.mnemonic() {
        Mnemonic::Fadd => exec_binop_reg_variants(x87, dest, src, |x87, i| x87.fadd_st0_sti(i), |x87, i| x87.fadd_sti_st0(i)),
        Mnemonic::Fsub => exec_binop_reg_variants(x87, dest, src, |x87, i| x87.fsub_st0_sti(i), |x87, i| x87.fsub_sti_st0(i)),
        Mnemonic::Fsubr => exec_binop_reg_variants(x87, dest, src, |x87, i| x87.fsubr_st0_sti(i), |x87, i| x87.fsubr_sti_st0(i)),
        Mnemonic::Fmul => exec_binop_reg_variants(x87, dest, src, |x87, i| x87.fmul_st0_sti(i), |x87, i| x87.fmul_sti_st0(i)),
        Mnemonic::Fdiv => exec_binop_reg_variants(x87, dest, src, |x87, i| x87.fdiv_st0_sti(i), |x87, i| x87.fdiv_sti_st0(i)),
        Mnemonic::Fdivr => exec_binop_reg_variants(x87, dest, src, |x87, i| x87.fdivr_st0_sti(i), |x87, i| x87.fdivr_sti_st0(i)),
        _ => Err(Exception::InvalidOpcode),
    }
}

fn exec_binop_reg_variants<F0, F1>(
    x87: &mut X87,
    dest: usize,
    src: usize,
    st0_sti: F0,
    sti_st0: F1,
) -> Result<(), Exception>
where
    F0: FnOnce(&mut crate::interp::x87::X87, usize) -> crate::interp::x87::Result<()>,
    F1: FnOnce(&mut crate::interp::x87::X87, usize) -> crate::interp::x87::Result<()>,
{
    if dest == 0 {
        st0_sti(x87, src).map_err(map_x87_fault)?;
        return Ok(());
    }
    if src == 0 {
        sti_st0(x87, dest).map_err(map_x87_fault)?;
        return Ok(());
    }
    Err(Exception::InvalidOpcode)
}

fn exec_binop_pop(x87: &mut X87, instr: &Instruction) -> Result<(), Exception> {
    let dest = if instr.op_count() == 0 {
        1usize
    } else {
        let reg = instr.op0_register();
        st_index(reg).ok_or(Exception::InvalidOpcode)?
    };
    match instr.mnemonic() {
        Mnemonic::Faddp => x87.faddp_sti_st0(dest),
        Mnemonic::Fsubp => x87.fsubp_sti_st0(dest),
        Mnemonic::Fsubrp => x87.fsubrp_sti_st0(dest),
        Mnemonic::Fmulp => x87.fmulp_sti_st0(dest),
        Mnemonic::Fdivp => x87.fdivp_sti_st0(dest),
        Mnemonic::Fdivrp => x87.fdivrp_sti_st0(dest),
        _ => return Err(Exception::InvalidOpcode),
    }
    .map_err(map_x87_fault)?;
    Ok(())
}

fn exec_compare<B: CpuBus>(
    state: &mut CpuState,
    x87: &mut X87,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<(), Exception> {
    match instr.op_kind(0) {
        OpKind::Register => {
            let i = st_index(instr.op0_register()).ok_or(Exception::InvalidOpcode)?;
            if instr.mnemonic() == Mnemonic::Fcom {
                x87.fcom_sti(i).map_err(map_x87_fault)?;
            } else {
                x87.fcomp_sti(i).map_err(map_x87_fault)?;
            }
        }
        OpKind::Memory => {
            let addr = memory_addr(state, instr, next_ip)?;
            match instr.memory_size() {
                MemorySize::Float32 => {
                    let v = read_mem_f32(bus, addr)?;
                    if instr.mnemonic() == Mnemonic::Fcom {
                        x87.fcom_m32(v).map_err(map_x87_fault)?;
                    } else {
                        x87.fcomp_m32(v).map_err(map_x87_fault)?;
                    }
                }
                MemorySize::Float64 => {
                    let v = read_mem_f64(bus, addr)?;
                    if instr.mnemonic() == Mnemonic::Fcom {
                        x87.fcom_m64(v).map_err(map_x87_fault)?;
                    } else {
                        x87.fcomp_m64(v).map_err(map_x87_fault)?;
                    }
                }
                _ => return Err(Exception::InvalidOpcode),
            }
        }
        _ => return Err(Exception::InvalidOpcode),
    }
    Ok(())
}

fn exec_comparei(state: &mut CpuState, x87: &mut X87, instr: &Instruction) -> Result<(), Exception> {
    let i = match instr.op_count() {
        1 => st_index(instr.op0_register()).ok_or(Exception::InvalidOpcode)?,
        2 => {
            let a = st_index(instr.op0_register()).ok_or(Exception::InvalidOpcode)?;
            let b = st_index(instr.op1_register()).ok_or(Exception::InvalidOpcode)?;
            if a == 0 {
                b
            } else if b == 0 {
                a
            } else {
                return Err(Exception::InvalidOpcode);
            }
        }
        _ => return Err(Exception::InvalidOpcode),
    };

    let flags = match instr.mnemonic() {
        Mnemonic::Fcomi => x87.fcomi_sti(i),
        Mnemonic::Fcomip => x87.fcomip_sti(i),
        Mnemonic::Fucomi => x87.fucomi_sti(i),
        Mnemonic::Fucomip => x87.fucomip_sti(i),
        _ => return Err(Exception::InvalidOpcode),
    }
    .map_err(map_x87_fault)?;

    let mut rflags = state.rflags();
    rflags &= !(FLAG_CF | FLAG_PF | FLAG_ZF | FLAG_OF | FLAG_SF | FLAG_AF);
    if flags.cf {
        rflags |= FLAG_CF;
    }
    if flags.pf {
        rflags |= FLAG_PF;
    }
    if flags.zf {
        rflags |= FLAG_ZF;
    }
    state.set_rflags(rflags);

    Ok(())
}

fn exec_fnstsw<B: CpuBus>(
    state: &mut CpuState,
    x87: &mut X87,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<(), Exception> {
    let sw = x87.fnstsw();
    if instr.op_count() == 0 {
        state.write_reg(Register::AX, sw as u64);
        return Ok(());
    }

    match instr.op_kind(0) {
        OpKind::Register => {
            state.write_reg(instr.op0_register(), sw as u64);
            Ok(())
        }
        OpKind::Memory => {
            let addr = memory_addr(state, instr, next_ip)?;
            bus.write_u16(addr, sw)?;
            Ok(())
        }
        _ => Err(Exception::InvalidOpcode),
    }
}

fn exec_fxch(x87: &mut X87, instr: &Instruction) -> Result<(), Exception> {
    let i = match instr.op_count() {
        0 => 1usize,
        1 => st_index(instr.op0_register()).ok_or(Exception::InvalidOpcode)?,
        2 => {
            let a = st_index(instr.op0_register()).ok_or(Exception::InvalidOpcode)?;
            let b = st_index(instr.op1_register()).ok_or(Exception::InvalidOpcode)?;
            if a == 0 {
                b
            } else if b == 0 {
                a
            } else {
                return Err(Exception::InvalidOpcode);
            }
        }
        _ => return Err(Exception::InvalidOpcode),
    };
    x87.fxch_sti(i).map_err(map_x87_fault)?;
    Ok(())
}

fn exec_ffree(x87: &mut X87, instr: &Instruction) -> Result<(), Exception> {
    let i = if instr.op_count() == 0 {
        0usize
    } else {
        st_index(instr.op0_register()).ok_or(Exception::InvalidOpcode)?
    };
    if instr.mnemonic() == Mnemonic::Ffreep {
        x87.ffreep_sti(i).map_err(map_x87_fault)?;
    } else {
        x87.ffree_sti(i).map_err(map_x87_fault)?;
    }
    Ok(())
}

fn x87_reg_pair(instr: &Instruction) -> Result<(usize, usize), Exception> {
    match instr.op_count() {
        1 => {
            let r = instr.op0_register();
            let i = st_index(r).ok_or(Exception::InvalidOpcode)?;
            Ok((0, i))
        }
        2 => {
            let d = st_index(instr.op0_register()).ok_or(Exception::InvalidOpcode)?;
            let s = st_index(instr.op1_register()).ok_or(Exception::InvalidOpcode)?;
            Ok((d, s))
        }
        _ => Err(Exception::InvalidOpcode),
    }
}
