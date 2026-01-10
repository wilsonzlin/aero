use crate::exception::Exception;
use crate::interp::x87::Fault as X87Fault;
use crate::mem::CpuBus;
use crate::state::{CpuState, FLAG_AF, FLAG_CF, FLAG_OF, FLAG_PF, FLAG_SF, FLAG_ZF};
use aero_x86::{DecodedInst, Instruction, MemorySize, Mnemonic, OpKind, Register};

use super::ops_data::calc_ea;
use super::ExecOutcome;

pub fn handles_mnemonic(m: Mnemonic) -> bool {
    matches!(
        m,
        Mnemonic::Fld
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
            | Mnemonic::Fucomi
            | Mnemonic::Fucomip
            | Mnemonic::Fldcw
            | Mnemonic::Fnstcw
            | Mnemonic::Fnstsw
    )
}

pub fn exec<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    decoded: &DecodedInst,
    next_ip: u64,
) -> Result<ExecOutcome, Exception> {
    let instr = &decoded.instr;
    match instr.mnemonic() {
        Mnemonic::Fld => {
            exec_fld(state, bus, instr, next_ip)?;
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Fst => {
            exec_fst(state, bus, instr, next_ip, false)?;
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Fstp => {
            exec_fst(state, bus, instr, next_ip, true)?;
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Fild => {
            exec_fild(state, bus, instr, next_ip)?;
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Fistp => {
            exec_fistp(state, bus, instr, next_ip)?;
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Fadd
        | Mnemonic::Fsub
        | Mnemonic::Fsubr
        | Mnemonic::Fmul
        | Mnemonic::Fdiv
        | Mnemonic::Fdivr => {
            exec_binop(state, bus, instr, next_ip)?;
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Faddp
        | Mnemonic::Fsubp
        | Mnemonic::Fsubrp
        | Mnemonic::Fmulp
        | Mnemonic::Fdivp
        | Mnemonic::Fdivrp => {
            exec_binop_pop(state, instr)?;
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Fchs => {
            state
                .x87_mut()
                .fchs()
                .map_err(map_x87_fault)?;
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Fabs => {
            state
                .x87_mut()
                .fabs()
                .map_err(map_x87_fault)?;
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Fcom | Mnemonic::Fcomp => {
            exec_compare(state, bus, instr, next_ip)?;
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Fcompp => {
            state
                .x87_mut()
                .fcompp()
                .map_err(map_x87_fault)?;
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Fucomi | Mnemonic::Fucomip => {
            exec_fucomi(state, instr)?;
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Fldcw => {
            let addr = memory_addr(state, instr, next_ip)?;
            let cw = bus.read_u16(addr)?;
            state.x87_mut().fldcw(cw);
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Fnstcw => {
            let addr = memory_addr(state, instr, next_ip)?;
            let cw = state.x87().fnstcw();
            bus.write_u16(addr, cw)?;
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Fnstsw => {
            exec_fnstsw(state, bus, instr, next_ip)?;
            Ok(ExecOutcome::Continue)
        }
        _ => Err(Exception::InvalidOpcode),
    }
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
                    state.x87_mut().fld_f32(v).map_err(map_x87_fault)?;
                }
                MemorySize::Float64 => {
                    let v = read_mem_f64(bus, addr)?;
                    state.x87_mut().fld_f64(v).map_err(map_x87_fault)?;
                }
                _ => return Err(Exception::InvalidOpcode),
            }
        }
        OpKind::Register => {
            let reg = instr.op0_register();
            let i = st_index(reg).ok_or(Exception::InvalidOpcode)?;
            state.x87_mut().fld_st(i).map_err(map_x87_fault)?;
        }
        _ => return Err(Exception::InvalidOpcode),
    }
    Ok(())
}

fn exec_fst<B: CpuBus>(
    state: &mut CpuState,
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
                        state.x87_mut().fstp_f32().map_err(map_x87_fault)?
                    } else {
                        state.x87_mut().fst_f32().map_err(map_x87_fault)?
                    };
                    bus.write_u32(addr, v.to_bits())?;
                }
                MemorySize::Float64 => {
                    let v = if pop {
                        state.x87_mut().fstp_f64().map_err(map_x87_fault)?
                    } else {
                        state.x87_mut().fst_f64().map_err(map_x87_fault)?
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
                state.x87_mut().fstp_st(i).map_err(map_x87_fault)?;
            } else {
                state.x87_mut().fst_st(i).map_err(map_x87_fault)?;
            }
        }
        _ => return Err(Exception::InvalidOpcode),
    }
    Ok(())
}

fn exec_fild<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<(), Exception> {
    let addr = memory_addr(state, instr, next_ip)?;
    match instr.memory_size() {
        MemorySize::Int16 => {
            let v = bus.read_u16(addr)? as i16;
            state.x87_mut().fild_i16(v).map_err(map_x87_fault)?;
        }
        MemorySize::Int32 => {
            let v = bus.read_u32(addr)? as i32;
            state.x87_mut().fild_i32(v).map_err(map_x87_fault)?;
        }
        MemorySize::Int64 => {
            let v = bus.read_u64(addr)? as i64;
            state.x87_mut().fild_i64(v).map_err(map_x87_fault)?;
        }
        _ => return Err(Exception::InvalidOpcode),
    }
    Ok(())
}

fn exec_fistp<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<(), Exception> {
    let addr = memory_addr(state, instr, next_ip)?;
    match instr.memory_size() {
        MemorySize::Int16 => {
            let v = state.x87_mut().fistp_i16().map_err(map_x87_fault)?;
            bus.write_u16(addr, v as u16)?;
        }
        MemorySize::Int32 => {
            let v = state.x87_mut().fistp_i32().map_err(map_x87_fault)?;
            bus.write_u32(addr, v as u32)?;
        }
        _ => return Err(Exception::InvalidOpcode),
    }
    Ok(())
}

fn exec_binop<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<(), Exception> {
    match instr.op_kind(0) {
        OpKind::Memory => exec_binop_mem(state, bus, instr, next_ip),
        OpKind::Register => exec_binop_reg(state, instr),
        _ => Err(Exception::InvalidOpcode),
    }
}

fn exec_binop_mem<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<(), Exception> {
    let addr = memory_addr(state, instr, next_ip)?;
    match instr.memory_size() {
        MemorySize::Float32 => {
            let v = read_mem_f32(bus, addr)?;
            match instr.mnemonic() {
                Mnemonic::Fadd => state.x87_mut().fadd_m32(v),
                Mnemonic::Fsub => state.x87_mut().fsub_m32(v),
                Mnemonic::Fsubr => state.x87_mut().fsubr_m32(v),
                Mnemonic::Fmul => state.x87_mut().fmul_m32(v),
                Mnemonic::Fdiv => state.x87_mut().fdiv_m32(v),
                Mnemonic::Fdivr => state.x87_mut().fdivr_m32(v),
                _ => return Err(Exception::InvalidOpcode),
            }
            .map_err(map_x87_fault)?;
        }
        MemorySize::Float64 => {
            let v = read_mem_f64(bus, addr)?;
            match instr.mnemonic() {
                Mnemonic::Fadd => state.x87_mut().fadd_m64(v),
                Mnemonic::Fsub => state.x87_mut().fsub_m64(v),
                Mnemonic::Fsubr => state.x87_mut().fsubr_m64(v),
                Mnemonic::Fmul => state.x87_mut().fmul_m64(v),
                Mnemonic::Fdiv => state.x87_mut().fdiv_m64(v),
                Mnemonic::Fdivr => state.x87_mut().fdivr_m64(v),
                _ => return Err(Exception::InvalidOpcode),
            }
            .map_err(map_x87_fault)?;
        }
        _ => return Err(Exception::InvalidOpcode),
    }
    Ok(())
}

fn exec_binop_reg(state: &mut CpuState, instr: &Instruction) -> Result<(), Exception> {
    let (dest, src) = x87_reg_pair(instr)?;
    match instr.mnemonic() {
        Mnemonic::Fadd => exec_binop_reg_variants(state, dest, src, |x87, i| x87.fadd_st0_sti(i), |x87, i| x87.fadd_sti_st0(i)),
        Mnemonic::Fsub => exec_binop_reg_variants(state, dest, src, |x87, i| x87.fsub_st0_sti(i), |x87, i| x87.fsub_sti_st0(i)),
        Mnemonic::Fsubr => exec_binop_reg_variants(state, dest, src, |x87, i| x87.fsubr_st0_sti(i), |x87, i| x87.fsubr_sti_st0(i)),
        Mnemonic::Fmul => exec_binop_reg_variants(state, dest, src, |x87, i| x87.fmul_st0_sti(i), |x87, i| x87.fmul_sti_st0(i)),
        Mnemonic::Fdiv => exec_binop_reg_variants(state, dest, src, |x87, i| x87.fdiv_st0_sti(i), |x87, i| x87.fdiv_sti_st0(i)),
        Mnemonic::Fdivr => exec_binop_reg_variants(state, dest, src, |x87, i| x87.fdivr_st0_sti(i), |x87, i| x87.fdivr_sti_st0(i)),
        _ => Err(Exception::InvalidOpcode),
    }
}

fn exec_binop_reg_variants<F0, F1>(
    state: &mut CpuState,
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
        st0_sti(state.x87_mut(), src).map_err(map_x87_fault)?;
        return Ok(());
    }
    if src == 0 {
        sti_st0(state.x87_mut(), dest).map_err(map_x87_fault)?;
        return Ok(());
    }
    Err(Exception::InvalidOpcode)
}

fn exec_binop_pop(state: &mut CpuState, instr: &Instruction) -> Result<(), Exception> {
    let dest = if instr.op_count() == 0 {
        1usize
    } else {
        let reg = instr.op0_register();
        st_index(reg).ok_or(Exception::InvalidOpcode)?
    };
    match instr.mnemonic() {
        Mnemonic::Faddp => state.x87_mut().faddp_sti_st0(dest),
        Mnemonic::Fsubp => state.x87_mut().fsubp_sti_st0(dest),
        Mnemonic::Fsubrp => state.x87_mut().fsubrp_sti_st0(dest),
        Mnemonic::Fmulp => state.x87_mut().fmulp_sti_st0(dest),
        Mnemonic::Fdivp => state.x87_mut().fdivp_sti_st0(dest),
        Mnemonic::Fdivrp => state.x87_mut().fdivrp_sti_st0(dest),
        _ => return Err(Exception::InvalidOpcode),
    }
    .map_err(map_x87_fault)?;
    Ok(())
}

fn exec_compare<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<(), Exception> {
    match instr.op_kind(0) {
        OpKind::Register => {
            let i = st_index(instr.op0_register()).ok_or(Exception::InvalidOpcode)?;
            if instr.mnemonic() == Mnemonic::Fcom {
                state.x87_mut().fcom_sti(i).map_err(map_x87_fault)?;
            } else {
                state.x87_mut().fcomp_sti(i).map_err(map_x87_fault)?;
            }
        }
        OpKind::Memory => {
            let addr = memory_addr(state, instr, next_ip)?;
            match instr.memory_size() {
                MemorySize::Float32 => {
                    let v = read_mem_f32(bus, addr)?;
                    if instr.mnemonic() == Mnemonic::Fcom {
                        state.x87_mut().fcom_m32(v).map_err(map_x87_fault)?;
                    } else {
                        state.x87_mut().fcomp_m32(v).map_err(map_x87_fault)?;
                    }
                }
                MemorySize::Float64 => {
                    let v = read_mem_f64(bus, addr)?;
                    if instr.mnemonic() == Mnemonic::Fcom {
                        state.x87_mut().fcom_m64(v).map_err(map_x87_fault)?;
                    } else {
                        state.x87_mut().fcomp_m64(v).map_err(map_x87_fault)?;
                    }
                }
                _ => return Err(Exception::InvalidOpcode),
            }
        }
        _ => return Err(Exception::InvalidOpcode),
    }
    Ok(())
}

fn exec_fucomi(state: &mut CpuState, instr: &Instruction) -> Result<(), Exception> {
    let i = if instr.op_count() == 1 {
        st_index(instr.op0_register()).ok_or(Exception::InvalidOpcode)?
    } else if instr.op_count() == 2 {
        // iced-x86 can emit FUCOMI ST0, ST(i)
        st_index(instr.op1_register()).ok_or(Exception::InvalidOpcode)?
    } else {
        return Err(Exception::InvalidOpcode);
    };

    let flags = match instr.mnemonic() {
        Mnemonic::Fucomi => state.x87_mut().fucomi_sti(i),
        Mnemonic::Fucomip => state.x87_mut().fucomip_sti(i),
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
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<(), Exception> {
    let sw = state.x87().fnstsw();
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
