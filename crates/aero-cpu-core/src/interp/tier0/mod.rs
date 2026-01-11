//! Tier-0 interpreter.
//!
//! This is the default interpreter for `aero_cpu_core` and operates directly on
//! the canonical JIT-ABI [`crate::state::CpuState`] + [`crate::mem::CpuBus`].
//! Higher tiers (JIT) build on the same state layout.

pub mod exec;

mod ops_alu;
mod ops_atomic;
mod ops_atomics;
mod ops_cf;
mod ops_data;
mod ops_fx;
mod ops_sse;
mod ops_string;
mod ops_x87;

use crate::cpuid::{CpuFeatureSet, CpuFeatures};
use crate::exception::{AssistReason, Exception};
use crate::fpu::FpKind;
use crate::linear_mem::{contiguous_masked_start, write_u16_wrapped, write_u32_wrapped, write_u64_wrapped};
use crate::mem::CpuBus;
use crate::state::{mask_bits, CpuState, CR0_EM, CR0_MP, CR0_NE, CR0_TS, CR4_OSFXSR};
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

impl Tier0Config {
    /// Construct a Tier-0 configuration from the guest-visible CPUID surface.
    ///
    /// Tier-0 instruction gating must match what `CPUID` advertises to the guest
    /// (via [`crate::assist::AssistContext`]). Use this helper to keep Tier-0
    /// coherent with the `CpuFeatures` policy.
    pub fn from_cpuid(features: &CpuFeatures) -> Self {
        Self {
            features: features.feature_set(),
        }
    }
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
    /// Like [`ExecOutcome::Continue`], but requests that the execution engine
    /// inhibit maskable interrupts for exactly one instruction (MOV SS / POP SS
    /// interrupt shadow semantics).
    ContinueInhibitInterrupts,
    Branch,
    Halt,
    Assist(AssistReason),
}

fn atomic_rmw_sized<B: CpuBus, R>(
    state: &CpuState,
    bus: &mut B,
    addr: u64,
    bits: u32,
    f: impl FnOnce(u64) -> (u64, R),
) -> Result<R, Exception> {
    let len = usize::try_from(bits / 8).map_err(|_| Exception::InvalidOpcode)?;
    if let Some(start) = contiguous_masked_start(state, addr, len) {
        return match bits {
            8 => bus.atomic_rmw::<u8, _>(start, |old| {
                let (new, ret) = f(old as u64);
                (new as u8, ret)
            }),
            16 => bus.atomic_rmw::<u16, _>(start, |old| {
                let (new, ret) = f(old as u64);
                (new as u16, ret)
            }),
            32 => bus.atomic_rmw::<u32, _>(start, |old| {
                let (new, ret) = f(old as u64);
                (new as u32, ret)
            }),
            64 => bus.atomic_rmw::<u64, _>(start, |old| f(old)),
            _ => Err(Exception::InvalidOpcode),
        };
    }

    // Wrapped (split) path: this is required when the linear address range wraps
    // (32-bit wrap in non-long modes, A20 alias wrap in real/v8086 with A20 off).
    // We cannot use `CpuBus::atomic_rmw` because it assumes a contiguous linear
    // range starting at `addr`.
    //
    // `atomic_rmw` also has write-intent semantics even when `new == old`. To
    // preserve that behavior here, read each byte through `CpuBus::atomic_rmw`
    // (so paging-aware busses perform write-intent translation/permission
    // checks) while still applying `CpuState::apply_a20` per byte.
    let mut old = 0u64;
    for i in 0..len {
        let byte_addr = state.apply_a20(addr.wrapping_add(i as u64));
        let b = bus.atomic_rmw::<u8, _>(byte_addr, |old| (old, old))?;
        old |= (b as u64) << (i * 8);
    }
    let mask = mask_bits(bits);
    old &= mask;

    let (new, ret) = f(old);
    let new = new & mask;
    if new != old {
        match bits {
            8 => bus.write_u8(state.apply_a20(addr), new as u8)?,
            16 => write_u16_wrapped(state, bus, addr, new as u16)?,
            32 => write_u32_wrapped(state, bus, addr, new as u32)?,
            64 => write_u64_wrapped(state, bus, addr, new)?,
            _ => return Err(Exception::InvalidOpcode),
        }
    }
    Ok(ret)
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
        Mnemonic::Clts => {
            // CLTS is privileged (CPL0). In real mode `cpl()` is always 0.
            if state.cpl() != 0 {
                return Err(Exception::gp0());
            }
            state.control.cr0 &= !CR0_TS;
            Ok(ExecOutcome::Continue)
        }
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

    // Give #UD priority over #NM: if the ISA is disabled entirely, we do not
    // report it as a lazy-FPU trap.
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
