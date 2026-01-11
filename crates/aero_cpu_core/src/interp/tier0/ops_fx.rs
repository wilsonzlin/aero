use crate::exception::Exception;
use crate::mem::CpuBus;
use crate::state::{CpuState, CR0_EM, CR0_TS, CR4_OSFXSR, CR4_OSXMMEXCPT};
use aero_x86::{DecodedInst, Mnemonic, OpKind};

use super::ops_data::calc_ea;
use super::ExecOutcome;

/// MXCSR exception mask bits (IM/DM/ZM/OM/UM/PM).
const MXCSR_EXCEPTION_MASK: u32 = 0x1F80;

pub fn handles_mnemonic(m: Mnemonic) -> bool {
    matches!(
        m,
        Mnemonic::Fxsave
            | Mnemonic::Fxrstor
            | Mnemonic::Fxsave64
            | Mnemonic::Fxrstor64
            | Mnemonic::Stmxcsr
            | Mnemonic::Ldmxcsr
    )
}

pub fn exec<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    decoded: &DecodedInst,
    next_ip: u64,
) -> Result<ExecOutcome, Exception> {
    let instr = &decoded.instr;

    // All of these opcodes are gated on CR0/CR4 in the same way.
    check_fx_available(state)?;

    let OpKind::Memory = instr.op_kind(0) else {
        return Err(Exception::InvalidOpcode);
    };

    let addr = calc_ea(state, instr, next_ip, true)?;

    match instr.mnemonic() {
        Mnemonic::Stmxcsr => {
            if addr & 0b11 != 0 {
                return Err(Exception::gp0());
            }
            bus.write_u32(addr, state.sse.mxcsr)?;
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Ldmxcsr => {
            if addr & 0b11 != 0 {
                return Err(Exception::gp0());
            }
            let mut mxcsr = bus.read_u32(addr)?;
            if (state.control.cr4 & CR4_OSXMMEXCPT) == 0 {
                mxcsr |= MXCSR_EXCEPTION_MASK;
            }
            state.sse.set_mxcsr(mxcsr)?;
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Fxsave | Mnemonic::Fxsave64 => {
            if addr & 0xF != 0 {
                return Err(Exception::gp0());
            }

            let mut image = [0u8; crate::FXSAVE_AREA_SIZE];
            match instr.mnemonic() {
                Mnemonic::Fxsave => state.fxsave32(&mut image),
                Mnemonic::Fxsave64 => state.fxsave64(&mut image),
                _ => unreachable!(),
            }

            for i in 0..(crate::FXSAVE_AREA_SIZE / 16) {
                let start = i * 16;
                let chunk: [u8; 16] = image[start..start + 16].try_into().unwrap();
                bus.write_u128(addr + start as u64, u128::from_le_bytes(chunk))?;
            }

            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Fxrstor | Mnemonic::Fxrstor64 => {
            if addr & 0xF != 0 {
                return Err(Exception::gp0());
            }

            let mut image = [0u8; crate::FXSAVE_AREA_SIZE];
            for i in 0..(crate::FXSAVE_AREA_SIZE / 16) {
                let start = i * 16;
                let v = bus.read_u128(addr + start as u64)?;
                image[start..start + 16].copy_from_slice(&v.to_le_bytes());
            }

            if (state.control.cr4 & CR4_OSXMMEXCPT) == 0 {
                let mxcsr = u32::from_le_bytes(image[24..28].try_into().unwrap());
                let forced = mxcsr | MXCSR_EXCEPTION_MASK;
                image[24..28].copy_from_slice(&forced.to_le_bytes());
            }

            match instr.mnemonic() {
                Mnemonic::Fxrstor => state.fxrstor32(&image)?,
                Mnemonic::Fxrstor64 => state.fxrstor64(&image)?,
                _ => unreachable!(),
            }

            Ok(ExecOutcome::Continue)
        }
        _ => Err(Exception::InvalidOpcode),
    }
}

fn check_fx_available(state: &CpuState) -> Result<(), Exception> {
    let cr0 = state.control.cr0;
    if (cr0 & CR0_EM) != 0 {
        return Err(Exception::InvalidOpcode);
    }
    if (cr0 & CR0_TS) != 0 {
        return Err(Exception::DeviceNotAvailable);
    }
    let cr4 = state.control.cr4;
    if (cr4 & CR4_OSFXSR) == 0 {
        return Err(Exception::InvalidOpcode);
    }
    Ok(())
}

