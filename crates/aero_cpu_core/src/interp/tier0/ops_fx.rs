use crate::exception::Exception;
use crate::fpu::FpKind;
use crate::mem::CpuBus;
use crate::state::CpuState;
use aero_x86::{DecodedInst, Mnemonic, OpKind};

use super::ops_data::calc_ea;
use super::ExecOutcome;

pub fn handles_mnemonic(m: Mnemonic) -> bool {
    matches!(
        m,
        Mnemonic::Fxsave
            | Mnemonic::Fxrstor
            | Mnemonic::Fxsave64
            | Mnemonic::Fxrstor64
            | Mnemonic::Stmxcsr
            | Mnemonic::Ldmxcsr
            | Mnemonic::Emms
    )
}

pub fn exec<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    decoded: &DecodedInst,
    next_ip: u64,
) -> Result<ExecOutcome, Exception> {
    let instr = &decoded.instr;

    if instr.mnemonic() == Mnemonic::Emms {
        // EMMS is MMX state management (x87 tag word). It does not depend on
        // CR4.OSFXSR, but is still gated by CR0.EM/CR0.TS.
        super::check_fp_available(state, FpKind::X87)?;
        state.emms();
        return Ok(ExecOutcome::Continue);
    }

    // FXSAVE/FXRSTOR + MXCSR operations are SSE state management.
    super::check_fp_available(state, FpKind::Sse)?;

    let OpKind::Memory = instr.op_kind(0) else {
        return Err(Exception::InvalidOpcode);
    };

    let addr = calc_ea(state, instr, next_ip, true)?;

    match instr.mnemonic() {
        Mnemonic::Stmxcsr => state.stmxcsr_to_mem(bus, addr).map(|()| ExecOutcome::Continue),
        Mnemonic::Ldmxcsr => state
            .ldmxcsr_from_mem(bus, addr)
            .map(|()| ExecOutcome::Continue),
        Mnemonic::Fxsave => state.fxsave_to_mem(bus, addr).map(|()| ExecOutcome::Continue),
        Mnemonic::Fxsave64 => state
            .fxsave64_to_mem(bus, addr)
            .map(|()| ExecOutcome::Continue),
        Mnemonic::Fxrstor => state
            .fxrstor_from_mem(bus, addr)
            .map(|()| ExecOutcome::Continue),
        Mnemonic::Fxrstor64 => state
            .fxrstor64_from_mem(bus, addr)
            .map(|()| ExecOutcome::Continue),
        _ => Err(Exception::InvalidOpcode),
    }
}
