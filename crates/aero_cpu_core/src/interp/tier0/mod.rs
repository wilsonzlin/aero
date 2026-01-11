//! Tier-0 interpreter.
//!
//! This is the default interpreter for `aero_cpu_core` and operates directly on
//! the canonical JIT-ABI [`crate::state::CpuState`] + [`crate::mem::CpuBus`].
//! Higher tiers (JIT) build on the same state layout.

pub mod exec;

mod ops_atomic;
mod ops_alu;
mod ops_atomics;
mod ops_cf;
mod ops_data;
mod ops_fx;
mod ops_sse;
mod ops_string;
mod ops_x87;

use crate::cpuid::CpuFeatureSet;
use crate::exception::{AssistReason, Exception};
use crate::fpu::FpKind;
use crate::mem::CpuBus;
use crate::state::{CpuState, CR0_EM, CR0_MP, CR0_NE, CR0_TS, CR4_OSFXSR};
use aero_x86::{DecodedInst, Mnemonic};

/// Configuration inputs for the Tier-0 interpreter.
///
/// Tier-0 executes directly against [`crate::state::CpuState`], so CPU-wide
/// knobs like CPUID feature reporting live outside the architectural state and
/// are plumbed in via this config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tier0Config {
    pub features: CpuFeatureSet,
}

impl Default for Tier0Config {
    fn default() -> Self {
        Self {
            // Tier-0 defaults to the minimum viable Win7 x86-64 profile.
            // Individual tests can override this to exercise optional features.
            features: CpuFeatureSet::win7_minimum(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExecOutcome {
    Continue,
    Branch,
    Halt,
    Assist(AssistReason),
}

fn atomic_rmw_sized<B: CpuBus, R>(
    bus: &mut B,
    addr: u64,
    bits: u32,
    f: impl FnOnce(u64) -> (u64, R),
) -> Result<R, Exception> {
    match bits {
        8 => bus.atomic_rmw::<u8, _>(addr, |old| {
            let (new, ret) = f(old as u64);
            (new as u8, ret)
        }),
        16 => bus.atomic_rmw::<u16, _>(addr, |old| {
            let (new, ret) = f(old as u64);
            (new as u16, ret)
        }),
        32 => bus.atomic_rmw::<u32, _>(addr, |old| {
            let (new, ret) = f(old as u64);
            (new as u32, ret)
        }),
        64 => bus.atomic_rmw::<u64, _>(addr, |old| f(old)),
        _ => Err(Exception::InvalidOpcode),
    }
}

fn exec_decoded<B: CpuBus>(
    cfg: &Tier0Config,
    state: &mut CpuState,
    bus: &mut B,
    decoded: &DecodedInst,
    next_ip: u64,
    addr_size_override: bool,
) -> Result<ExecOutcome, Exception> {
    let mnem = decoded.instr.mnemonic();
    if decoded.instr.has_lock_prefix() && !mnemonic_allows_lock_prefix(mnem) {
        return Err(Exception::InvalidOpcode);
    }
    if ops_atomics::handles_mnemonic(mnem) {
        return ops_atomics::exec(state, bus, decoded, next_ip);
    }
    if ops_cf::handles_mnemonic(mnem) {
        return ops_cf::exec(state, bus, decoded, next_ip);
    }
    if ops_atomic::handles_mnemonic(mnem) {
        return ops_atomic::exec(state, bus, decoded, next_ip);
    }
    if ops_data::handles_mnemonic(mnem) {
        return ops_data::exec(state, bus, decoded, next_ip);
    }
    if ops_alu::handles_mnemonic(mnem) {
        return ops_alu::exec(state, bus, decoded, next_ip);
    }
    if ops_fx::handles_mnemonic(mnem) {
        return ops_fx::exec(state, bus, decoded, next_ip);
    }
    if ops_x87::handles_mnemonic(mnem) {
        return ops_x87::exec(state, bus, decoded, next_ip);
    }
    if ops_string::handles(&decoded.instr) {
        return ops_string::exec(state, bus, decoded, next_ip, addr_size_override);
    }
    if ops_sse::handles_mnemonic(mnem) {
        return ops_sse::exec(cfg, state, bus, decoded, next_ip);
    }

    match mnem {
        Mnemonic::Wait => {
            exec_wait(state)?;
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Hlt => {
            // HLT is privileged outside real mode.
            if state.cpl() != 0 {
                return Err(Exception::gp0());
            }
            Ok(ExecOutcome::Halt)
        }
        Mnemonic::In
        | Mnemonic::Out
        | Mnemonic::Insb
        | Mnemonic::Insw
        | Mnemonic::Insd
        | Mnemonic::Outsb
        | Mnemonic::Outsw
        | Mnemonic::Outsd => Ok(ExecOutcome::Assist(AssistReason::Io)),
        Mnemonic::Cpuid => Ok(ExecOutcome::Assist(AssistReason::Cpuid)),
        Mnemonic::Rdmsr | Mnemonic::Wrmsr => Ok(ExecOutcome::Assist(AssistReason::Msr)),
        Mnemonic::Int | Mnemonic::Int1 | Mnemonic::Int3 | Mnemonic::Into => {
            Ok(ExecOutcome::Assist(AssistReason::Interrupt))
        }
        Mnemonic::Iret | Mnemonic::Iretd | Mnemonic::Iretq => {
            Ok(ExecOutcome::Assist(AssistReason::Interrupt))
        }
        Mnemonic::Cli | Mnemonic::Sti => Ok(ExecOutcome::Assist(AssistReason::Interrupt)),
        // Privileged/system instructions that require additional CPU core state.
        Mnemonic::Lgdt
        | Mnemonic::Lidt
        | Mnemonic::Sgdt
        | Mnemonic::Sidt
        | Mnemonic::Ltr
        | Mnemonic::Str
        | Mnemonic::Lldt
        | Mnemonic::Sldt
        | Mnemonic::Lmsw
        | Mnemonic::Smsw
        | Mnemonic::Invlpg
        | Mnemonic::Swapgs
        | Mnemonic::Syscall
        | Mnemonic::Sysret
        | Mnemonic::Sysenter
        | Mnemonic::Sysexit
        | Mnemonic::Rsm => Ok(ExecOutcome::Assist(AssistReason::Privileged)),
        Mnemonic::Rdtsc
        | Mnemonic::Rdtscp
        | Mnemonic::Lfence
        | Mnemonic::Sfence
        | Mnemonic::Mfence => Ok(ExecOutcome::Assist(AssistReason::Unsupported)),
        _ => Err(Exception::InvalidOpcode),
    }
}

fn mnemonic_allows_lock_prefix(m: Mnemonic) -> bool {
    matches!(
        m,
        Mnemonic::Add
            | Mnemonic::Adc
            | Mnemonic::And
            | Mnemonic::Btc
            | Mnemonic::Btr
            | Mnemonic::Bts
            | Mnemonic::Cmpxchg
            | Mnemonic::Cmpxchg8b
            | Mnemonic::Cmpxchg16b
            | Mnemonic::Dec
            | Mnemonic::Inc
            | Mnemonic::Neg
            | Mnemonic::Not
            | Mnemonic::Or
            | Mnemonic::Sbb
            | Mnemonic::Sub
            | Mnemonic::Xadd
            | Mnemonic::Xchg
            | Mnemonic::Xor
    )
}

pub(super) fn check_fp_available(state: &CpuState, kind: FpKind) -> Result<(), Exception> {
    let cr0 = state.control.cr0;

    if (cr0 & CR0_EM) != 0 {
        return Err(Exception::InvalidOpcode);
    }

    if matches!(kind, FpKind::Sse) && (state.control.cr4 & CR4_OSFXSR) == 0 {
        return Err(Exception::InvalidOpcode);
    }

    if (cr0 & CR0_TS) != 0 {
        return Err(Exception::DeviceNotAvailable);
    }

    Ok(())
}

pub(super) fn exec_wait(state: &mut CpuState) -> Result<(), Exception> {
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

        state.set_irq13_pending(true);
    }

    Ok(())
}
