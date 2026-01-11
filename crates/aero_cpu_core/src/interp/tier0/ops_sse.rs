use crate::exception::Exception;
use crate::fpu::FpKind;
use crate::mem::CpuBus;
use crate::state::CpuState;
use aero_x86::{DecodedInst, Instruction, Mnemonic, OpKind, Register};

use super::ops_data::calc_ea;
use super::ExecOutcome;

pub fn handles_mnemonic(m: Mnemonic) -> bool {
    matches!(m, Mnemonic::Xorps)
}

pub fn exec<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    decoded: &DecodedInst,
    next_ip: u64,
) -> Result<ExecOutcome, Exception> {
    let instr = &decoded.instr;
    match instr.mnemonic() {
        Mnemonic::Xorps => {
            super::check_fp_available(state, FpKind::Sse)?;
            exec_xorps(state, bus, instr, next_ip)?;
            Ok(ExecOutcome::Continue)
        }
        _ => Err(Exception::InvalidOpcode),
    }
}

fn xmm_index(reg: Register) -> Option<usize> {
    Some(match reg {
        Register::XMM0 => 0,
        Register::XMM1 => 1,
        Register::XMM2 => 2,
        Register::XMM3 => 3,
        Register::XMM4 => 4,
        Register::XMM5 => 5,
        Register::XMM6 => 6,
        Register::XMM7 => 7,
        Register::XMM8 => 8,
        Register::XMM9 => 9,
        Register::XMM10 => 10,
        Register::XMM11 => 11,
        Register::XMM12 => 12,
        Register::XMM13 => 13,
        Register::XMM14 => 14,
        Register::XMM15 => 15,
        _ => return None,
    })
}

fn exec_xorps<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<(), Exception> {
    if instr.op_kind(0) != OpKind::Register {
        return Err(Exception::InvalidOpcode);
    }
    let dst = xmm_index(instr.op0_register()).ok_or(Exception::InvalidOpcode)?;

    let src_val = match instr.op_kind(1) {
        OpKind::Register => {
            let src = xmm_index(instr.op1_register()).ok_or(Exception::InvalidOpcode)?;
            state.sse.xmm[src]
        }
        OpKind::Memory => {
            let addr = calc_ea(state, instr, next_ip, true)?;
            bus.read_u128(addr)?
        }
        _ => return Err(Exception::InvalidOpcode),
    };

    state.sse.xmm[dst] ^= src_val;
    Ok(())
}

