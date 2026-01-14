use aero_x86::{DecodedInst, Instruction, Mnemonic, OpKind, Register};

use crate::cpuid::{self, CpuFeatures};
use crate::exception::{AssistReason, Exception};
use crate::linear_mem::{
    fetch_wrapped, read_u16_wrapped, read_u32_wrapped, read_u64_wrapped, write_u16_wrapped,
    write_u32_wrapped, write_u64_wrapped,
};
use crate::mem::CpuBus;
use crate::msr;
use crate::segmentation::{LoadReason, Seg};
use crate::state::{mask_bits, CpuMode, CpuState, RFLAGS_IOPL_MASK};
use crate::time::TimeSource;

/// Maximum number of `INVLPG` addresses retained in [`AssistContext::invlpg_log`].
///
/// This log is only intended for tests/debugging; bounding it prevents unbounded
/// memory growth in long-running test workloads that execute many `INVLPG`s.
pub const MAX_INVLPG_LOG_ENTRIES: usize = 4096;

// Backwards-compat alias for the constant name used in older discussions/specs.
// (The "B" is a typo, but keeping it avoids churn in downstream code/tests.)
pub const MAX_INVLPGB_LOG_ENTRIES: usize = MAX_INVLPG_LOG_ENTRIES;

/// Runtime context needed by the Tier-0 assist layer.
///
/// Tier-0 is intentionally minimal and does not model system state like MSRs,
/// descriptor tables, or deterministic time. When it encounters an instruction
/// that depends on that state, it exits with [`AssistReason`]. The caller feeds
/// those exits into [`handle_assist`], which emulates the instruction against
/// the JIT ABI [`CpuState`].
#[derive(Debug, Clone, Default)]
pub struct AssistContext {
    /// CPUID feature policy used for `CPUID` and for masking MSR writes (e.g.
    /// keeping `IA32_EFER` coherent with advertised features).
    pub features: CpuFeatures,
    /// Optional log of `INVLPG` linear addresses (useful for integration tests).
    ///
    /// The log is bounded to [`MAX_INVLPG_LOG_ENTRIES`]. When the capacity is
    /// reached, new entries are dropped and
    /// [`AssistContext::dropped_invlpg_log_entries`] is incremented.
    pub invlpg_log: Vec<u64>,
    /// Number of `INVLPG` log entries dropped because [`AssistContext::invlpg_log`] hit
    /// [`AssistContext::INVLPG_LOG_CAP`].
    pub dropped_invlpg_log_entries: u64,
}

impl AssistContext {
    /// Hard cap on the number of `INVLPG` addresses recorded in [`AssistContext::invlpg_log`].
    ///
    /// This log is a debug/testing facility and must never grow without bound (guest kernels can
    /// execute `INVLPG` frequently).
    pub const INVLPG_LOG_CAP: usize = MAX_INVLPG_LOG_ENTRIES;

    #[inline]
    pub fn invlpg_log_dropped(&self) -> u64 {
        self.dropped_invlpg_log_entries
    }

    #[inline]
    pub fn clear_invlpg_log(&mut self) {
        self.invlpg_log.clear();
        self.dropped_invlpg_log_entries = 0;
    }

    #[inline]
    fn record_invlpg(&mut self, addr: u64) {
        if self.invlpg_log.len() < Self::INVLPG_LOG_CAP {
            self.invlpg_log.push(addr);
        } else {
            self.dropped_invlpg_log_entries = self.dropped_invlpg_log_entries.saturating_add(1);
        }
    }
}

/// Execute a Tier-0 assist exit.
///
/// The assist handler:
/// - fetches + decodes the faulting instruction at the current RIP
/// - executes its architectural semantics using [`CpuState`]
/// - advances RIP (or transfers control) so the caller can resume execution
pub fn handle_assist<B: CpuBus>(
    ctx: &mut AssistContext,
    time: &mut TimeSource,
    state: &mut CpuState,
    bus: &mut B,
    _reason: AssistReason,
) -> Result<(), Exception> {
    // Keep the bus paging/MMU view coherent with architectural state even when
    // `handle_assist` is used outside of `tier0::exec::step` / `run_batch_with_assists`.
    bus.sync(state);
    let ip = state.rip();
    let fetch_addr = state.apply_a20(state.seg_base_reg(Register::CS).wrapping_add(ip));
    let bytes = fetch_wrapped(state, bus, fetch_addr, 15)
        .inspect_err(|e| state.apply_exception_side_effects(e))?;
    let addr_size_override = has_addr_size_override(&bytes, state.bitness());
    let decoded = match aero_x86::decode(&bytes, ip, state.bitness()) {
        Ok(decoded) => decoded,
        Err(_) => {
            let e = Exception::InvalidOpcode;
            state.apply_exception_side_effects(&e);
            return Err(e);
        }
    };

    exec_decoded(ctx, time, state, bus, &decoded, addr_size_override)
        .inspect_err(|e| state.apply_exception_side_effects(e))?;
    // Assists can update paging-related state (CR0/CR3/CR4/EFER/CPL). Sync the
    // bus so the next instruction boundary observes those updates even if the
    // caller doesn't go through `tier0::exec::step` immediately.
    bus.sync(state);
    Ok(())
}

/// Execute an assist using a pre-decoded instruction.
///
/// This is used by Tier-0 execution glue that already fetched/decoded the
/// instruction bytes and wants to avoid an extra decode pass. Callers should
/// also supply the already-parsed address-size override prefix state so assists
/// like `INS*`/`OUTS*` can pick the correct implicit index/counter registers.
pub fn handle_assist_decoded<B: CpuBus>(
    ctx: &mut AssistContext,
    time: &mut TimeSource,
    state: &mut CpuState,
    bus: &mut B,
    decoded: &DecodedInst,
    addr_size_override: bool,
) -> Result<(), Exception> {
    exec_decoded(ctx, time, state, bus, decoded, addr_size_override)
        .inspect_err(|e| state.apply_exception_side_effects(e))
}

fn exec_decoded<B: CpuBus>(
    ctx: &mut AssistContext,
    time: &mut TimeSource,
    state: &mut CpuState,
    bus: &mut B,
    decoded: &DecodedInst,
    addr_size_override: bool,
) -> Result<(), Exception> {
    let instr = &decoded.instr;
    let ip = state.rip();
    let next_ip_raw = ip.wrapping_add(decoded.len as u64);

    match instr.mnemonic() {
        Mnemonic::Cpuid => {
            instr_cpuid(ctx, state);
            state.set_rip(next_ip_raw);
            Ok(())
        }
        Mnemonic::Rdmsr => {
            instr_rdmsr(ctx, time, state)?;
            state.set_rip(next_ip_raw);
            Ok(())
        }
        Mnemonic::Wrmsr => {
            instr_wrmsr(ctx, time, state)?;
            state.set_rip(next_ip_raw);
            Ok(())
        }
        Mnemonic::Rdtsc => {
            instr_rdtsc(time, state);
            state.set_rip(next_ip_raw);
            Ok(())
        }
        Mnemonic::Rdtscp => {
            instr_rdtscp(time, state);
            state.set_rip(next_ip_raw);
            Ok(())
        }
        Mnemonic::Lfence | Mnemonic::Sfence | Mnemonic::Mfence => {
            // Tier-0 uses assists for fence opcodes so the runtime can model them
            // as serializing points if needed. For now they are treated as NOPs.
            state.set_rip(next_ip_raw);
            Ok(())
        }
        Mnemonic::Pause => {
            // NOP with a spin-loop hint.
            core::hint::spin_loop();
            state.set_rip(next_ip_raw);
            Ok(())
        }
        Mnemonic::In | Mnemonic::Out => {
            instr_in_out(state, bus, instr)?;
            state.set_rip(next_ip_raw);
            Ok(())
        }
        Mnemonic::Insb | Mnemonic::Insw | Mnemonic::Insd => {
            instr_ins(state, bus, instr, addr_size_override)?;
            state.set_rip(next_ip_raw);
            Ok(())
        }
        Mnemonic::Outsb | Mnemonic::Outsw | Mnemonic::Outsd => {
            instr_outs(state, bus, instr, addr_size_override)?;
            state.set_rip(next_ip_raw);
            Ok(())
        }
        Mnemonic::Cli
        | Mnemonic::Sti
        | Mnemonic::Int
        | Mnemonic::Int1
        | Mnemonic::Int3
        | Mnemonic::Into
        | Mnemonic::Iret
        | Mnemonic::Iretd
        | Mnemonic::Iretq => Err(Exception::Unimplemented(
            "interrupt assist requires CpuCore",
        )),
        Mnemonic::Mov => {
            instr_mov_privileged(ctx, state, bus, instr, next_ip_raw)?;
            Ok(())
        }
        Mnemonic::Pop => {
            instr_pop_privileged(ctx, state, bus, instr)?;
            state.set_rip(next_ip_raw);
            Ok(())
        }
        Mnemonic::Jmp => {
            instr_jmp_far(ctx, state, bus, instr, next_ip_raw)?;
            Ok(())
        }
        Mnemonic::Call => {
            instr_call_far(ctx, state, bus, instr, next_ip_raw)?;
            Ok(())
        }
        Mnemonic::Retf => {
            instr_retf(ctx, state, bus, instr)?;
            Ok(())
        }
        Mnemonic::Lgdt | Mnemonic::Lidt => {
            instr_lgdt_lidt(state, bus, instr, next_ip_raw)?;
            state.set_rip(next_ip_raw);
            Ok(())
        }
        Mnemonic::Sgdt | Mnemonic::Sidt => {
            instr_sgdt_sidt(state, bus, instr, next_ip_raw)?;
            state.set_rip(next_ip_raw);
            Ok(())
        }
        Mnemonic::Ltr | Mnemonic::Lldt => {
            instr_ltr_lldt(ctx, state, bus, instr, next_ip_raw)?;
            state.set_rip(next_ip_raw);
            Ok(())
        }
        Mnemonic::Str | Mnemonic::Sldt => {
            instr_str_sldt(state, bus, instr, next_ip_raw)?;
            state.set_rip(next_ip_raw);
            Ok(())
        }
        Mnemonic::Lmsw | Mnemonic::Smsw => {
            instr_lmsw_smsw(state, bus, instr, next_ip_raw)?;
            state.set_rip(next_ip_raw);
            Ok(())
        }
        Mnemonic::Invlpg => {
            instr_invlpg(ctx, state, bus, instr, next_ip_raw)?;
            state.set_rip(next_ip_raw);
            Ok(())
        }
        Mnemonic::Swapgs => {
            instr_swapgs(state)?;
            state.set_rip(next_ip_raw);
            Ok(())
        }
        Mnemonic::Syscall => {
            instr_syscall(ctx, state, next_ip_raw)?;
            Ok(())
        }
        Mnemonic::Sysret => {
            instr_sysret(ctx, state)?;
            Ok(())
        }
        Mnemonic::Sysenter => {
            instr_sysenter(state)?;
            Ok(())
        }
        Mnemonic::Sysexit => {
            instr_sysexit(state)?;
            Ok(())
        }
        Mnemonic::Rsm => Err(Exception::InvalidOpcode),
        _ => Err(Exception::InvalidOpcode),
    }
}

// -------------------------------------------------------------------------------------------------
// Basic helpers
// -------------------------------------------------------------------------------------------------

fn require_cpl0(state: &CpuState) -> Result<(), Exception> {
    if state.cpl() != 0 {
        return Err(Exception::gp0());
    }
    Ok(())
}

fn require_iopl(state: &CpuState) -> Result<(), Exception> {
    let cpl = state.cpl();
    let iopl = ((state.rflags() & RFLAGS_IOPL_MASK) >> 12) as u8;
    if cpl > iopl {
        return Err(Exception::gp0());
    }
    Ok(())
}

fn is_seg_reg(reg: Register) -> bool {
    matches!(
        reg,
        Register::ES | Register::CS | Register::SS | Register::DS | Register::FS | Register::GS
    )
}

fn seg_from_reg(reg: Register) -> Result<Seg, Exception> {
    Ok(match reg {
        Register::ES => Seg::ES,
        Register::CS => Seg::CS,
        Register::SS => Seg::SS,
        Register::DS => Seg::DS,
        Register::FS => Seg::FS,
        Register::GS => Seg::GS,
        _ => return Err(Exception::InvalidOpcode),
    })
}

fn is_ctrl_reg(reg: Register) -> bool {
    matches!(
        reg,
        Register::CR0 | Register::CR2 | Register::CR3 | Register::CR4 | Register::CR8
    )
}

fn is_debug_reg(reg: Register) -> bool {
    matches!(
        reg,
        Register::DR0
            | Register::DR1
            | Register::DR2
            | Register::DR3
            | Register::DR6
            | Register::DR7
    )
}

fn reg_bits(reg: Register) -> Result<u32, Exception> {
    use Register::*;
    Ok(match reg {
        AL | CL | DL | BL | AH | CH | DH | BH | SPL | BPL | SIL | DIL | R8L | R9L | R10L | R11L
        | R12L | R13L | R14L | R15L => 8,
        AX | CX | DX | BX | SP | BP | SI | DI | R8W | R9W | R10W | R11W | R12W | R13W | R14W
        | R15W => 16,
        EAX | ECX | EDX | EBX | ESP | EBP | ESI | EDI | R8D | R9D | R10D | R11D | R12D | R13D
        | R14D | R15D => 32,
        RAX | RCX | RDX | RBX | RSP | RBP | RSI | RDI | R8 | R9 | R10 | R11 | R12 | R13 | R14
        | R15 => 64,
        _ => return Err(Exception::InvalidOpcode),
    })
}

fn io_size_from_reg(reg: Register) -> Result<u32, Exception> {
    let bits = reg_bits(reg)?;
    Ok(bits / 8)
}

fn calc_ea(
    state: &CpuState,
    instr: &Instruction,
    next_ip: u64,
    include_seg: bool,
) -> Result<u64, Exception> {
    let base = instr.memory_base();
    let index = instr.memory_index();
    let scale = instr.memory_index_scale() as u64;
    let mut disp = instr.memory_displacement64() as i128;
    if base == Register::RIP {
        disp -= next_ip as i128;
    }

    let addr_bits = if base == Register::RIP {
        64
    } else if base != Register::None {
        reg_bits(base)?
    } else if index != Register::None {
        reg_bits(index)?
    } else {
        match instr.memory_displ_size() {
            2 => 16,
            4 => 32,
            8 => 64,
            _ => state.bitness(),
        }
    };

    let mut offset: i128 = disp;
    if base != Register::None {
        let base_val = if base == Register::RIP {
            next_ip
        } else {
            state.read_reg(base)
        };
        offset += (base_val & mask_bits(addr_bits)) as i128;
    }
    if index != Register::None {
        let idx_val = state.read_reg(index) & mask_bits(addr_bits);
        offset += (idx_val as i128) * (scale as i128);
    }

    let addr = (offset as u64) & mask_bits(addr_bits);
    if include_seg {
        Ok(state.apply_a20(
            state
                .seg_base_reg(instr.memory_segment())
                .wrapping_add(addr),
        ))
    } else {
        Ok(addr)
    }
}

fn read_mem<B: CpuBus>(
    state: &CpuState,
    bus: &mut B,
    addr: u64,
    bits: u32,
) -> Result<u64, Exception> {
    match bits {
        8 => Ok(bus.read_u8(state.apply_a20(addr))? as u64),
        16 => Ok(read_u16_wrapped(state, bus, addr)? as u64),
        32 => Ok(read_u32_wrapped(state, bus, addr)? as u64),
        64 => Ok(read_u64_wrapped(state, bus, addr)?),
        _ => Err(Exception::InvalidOpcode),
    }
}

fn write_mem<B: CpuBus>(
    state: &CpuState,
    bus: &mut B,
    addr: u64,
    bits: u32,
    val: u64,
) -> Result<(), Exception> {
    match bits {
        8 => bus.write_u8(state.apply_a20(addr), val as u8),
        16 => write_u16_wrapped(state, bus, addr, val as u16),
        32 => write_u32_wrapped(state, bus, addr, val as u32),
        64 => write_u64_wrapped(state, bus, addr, val),
        _ => Err(Exception::InvalidOpcode),
    }
}

fn read_op_u16<B: CpuBus>(
    state: &CpuState,
    bus: &mut B,
    instr: &Instruction,
    op: u32,
    next_ip: u64,
) -> Result<u16, Exception> {
    match instr.op_kind(op) {
        OpKind::Register => Ok(state.read_reg(instr.op_register(op)) as u16),
        OpKind::Memory => {
            let addr = calc_ea(state, instr, next_ip, true)?;
            read_u16_wrapped(state, bus, addr)
        }
        OpKind::Immediate16 => Ok(instr.immediate16()),
        OpKind::Immediate8to16 => Ok(instr.immediate8to16() as u16),
        _ => Err(Exception::InvalidOpcode),
    }
}

fn write_op_u16<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    op: u32,
    value: u16,
    next_ip: u64,
) -> Result<(), Exception> {
    match instr.op_kind(op) {
        OpKind::Register => {
            state.write_reg(instr.op_register(op), value as u64);
            Ok(())
        }
        OpKind::Memory => {
            let addr = calc_ea(state, instr, next_ip, true)?;
            write_u16_wrapped(state, bus, addr, value)
        }
        _ => Err(Exception::InvalidOpcode),
    }
}

// -------------------------------------------------------------------------------------------------
// CPUID
// -------------------------------------------------------------------------------------------------

fn instr_cpuid(ctx: &AssistContext, state: &mut CpuState) {
    let leaf = state.read_reg(Register::EAX) as u32;
    let subleaf = state.read_reg(Register::ECX) as u32;
    let res = cpuid::cpuid(&ctx.features, leaf, subleaf);
    state.write_reg(Register::EAX, res.eax as u64);
    state.write_reg(Register::EBX, res.ebx as u64);
    state.write_reg(Register::ECX, res.ecx as u64);
    state.write_reg(Register::EDX, res.edx as u64);
}

// -------------------------------------------------------------------------------------------------
// MSRs
// -------------------------------------------------------------------------------------------------

fn msr_read(
    _ctx: &AssistContext,
    time: &mut TimeSource,
    state: &mut CpuState,
    msr_index: u32,
) -> Result<u64, Exception> {
    match msr_index {
        msr::IA32_TSC => {
            let tsc = time.read_tsc();
            state.msr.tsc = tsc;
            Ok(tsc)
        }
        _ => state.msr.read(msr_index),
    }
}

fn msr_write(
    ctx: &mut AssistContext,
    time: &mut TimeSource,
    state: &mut CpuState,
    msr_index: u32,
    value: u64,
) -> Result<(), Exception> {
    match msr_index {
        msr::IA32_EFER => {
            state.msr.write(&ctx.features, msr_index, value)?;
            state.update_mode();
            Ok(())
        }
        msr::IA32_FS_BASE => {
            state.msr.write(&ctx.features, msr_index, value)?;
            if state.mode == CpuMode::Long {
                state.segments.fs.base = value;
            }
            Ok(())
        }
        msr::IA32_GS_BASE => {
            state.msr.write(&ctx.features, msr_index, value)?;
            if state.mode == CpuMode::Long {
                state.segments.gs.base = value;
            }
            Ok(())
        }
        msr::IA32_TSC => {
            time.set_tsc(value);
            state.msr.tsc = value;
            Ok(())
        }
        _ => state.msr.write(&ctx.features, msr_index, value),
    }
}

fn instr_rdmsr(
    ctx: &AssistContext,
    time: &mut TimeSource,
    state: &mut CpuState,
) -> Result<(), Exception> {
    require_cpl0(state)?;
    let msr_index = state.read_reg(Register::ECX) as u32;
    let value = msr_read(ctx, time, state, msr_index)?;
    state.write_reg(Register::EAX, value as u32 as u64);
    state.write_reg(Register::EDX, (value >> 32) as u32 as u64);
    Ok(())
}

fn instr_wrmsr(
    ctx: &mut AssistContext,
    time: &mut TimeSource,
    state: &mut CpuState,
) -> Result<(), Exception> {
    require_cpl0(state)?;
    let msr_index = state.read_reg(Register::ECX) as u32;
    let eax = state.read_reg(Register::EAX) as u32 as u64;
    let edx = state.read_reg(Register::EDX) as u32 as u64;
    let value = (edx << 32) | eax;
    msr_write(ctx, time, state, msr_index, value)?;
    Ok(())
}

// -------------------------------------------------------------------------------------------------
// Time
// -------------------------------------------------------------------------------------------------

fn instr_rdtsc(time: &mut TimeSource, state: &mut CpuState) {
    let tsc = time.read_tsc();
    state.msr.tsc = tsc;
    state.write_reg(Register::EAX, tsc as u32 as u64);
    state.write_reg(Register::EDX, (tsc >> 32) as u32 as u64);
}

fn instr_rdtscp(time: &mut TimeSource, state: &mut CpuState) {
    let tsc = time.read_tsc();
    state.msr.tsc = tsc;
    state.write_reg(Register::EAX, tsc as u32 as u64);
    state.write_reg(Register::EDX, (tsc >> 32) as u32 as u64);
    state.write_reg(Register::ECX, state.msr.tsc_aux as u64);
}

// -------------------------------------------------------------------------------------------------
// Port I/O
// -------------------------------------------------------------------------------------------------

fn instr_in_out<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
) -> Result<(), Exception> {
    require_iopl(state)?;
    match instr.mnemonic() {
        Mnemonic::In => {
            let dst = instr.op0_register();
            let size = io_size_from_reg(dst)?;
            let port = match instr.op_kind(1) {
                OpKind::Immediate8 => instr.immediate8() as u16,
                OpKind::Register => state.read_reg(instr.op1_register()) as u16,
                _ => return Err(Exception::InvalidOpcode),
            };
            let val = bus.io_read(port, size)?;
            state.write_reg(dst, val);
            Ok(())
        }
        Mnemonic::Out => {
            let port = match instr.op_kind(0) {
                OpKind::Immediate8 => instr.immediate8() as u16,
                OpKind::Register => state.read_reg(instr.op0_register()) as u16,
                _ => return Err(Exception::InvalidOpcode),
            };
            let src = instr.op1_register();
            let size = io_size_from_reg(src)?;
            let val = state.read_reg(src);
            bus.io_write(port, size, val)?;
            Ok(())
        }
        _ => Err(Exception::InvalidOpcode),
    }
}

pub(crate) fn has_addr_size_override(bytes: &[u8; 15], bitness: u32) -> bool {
    let mut i = 0usize;
    let mut seen = false;
    while i < bytes.len() {
        let b = bytes[i];
        let is_legacy_prefix = matches!(
            b,
            0xF0 | 0xF2 | 0xF3 // lock/rep
                | 0x2E | 0x36 | 0x3E | 0x26 | 0x64 | 0x65 // segment overrides
                | 0x66 // operand-size override
                | 0x67 // address-size override
        );
        let is_rex = bitness == 64 && (0x40..=0x4F).contains(&b);
        if !(is_legacy_prefix || is_rex) {
            break;
        }
        if b == 0x67 {
            seen = true;
        }
        i += 1;
    }
    seen
}

fn effective_addr_size(state: &CpuState, addr_size_override: bool) -> Result<u32, Exception> {
    Ok(match state.bitness() {
        16 => {
            if addr_size_override {
                32
            } else {
                16
            }
        }
        32 => {
            if addr_size_override {
                16
            } else {
                32
            }
        }
        64 => {
            if addr_size_override {
                32
            } else {
                64
            }
        }
        _ => return Err(Exception::InvalidOpcode),
    })
}

fn string_count_reg(addr_bits: u32) -> Register {
    match addr_bits {
        16 => Register::CX,
        32 => Register::ECX,
        _ => Register::RCX,
    }
}

fn string_src_index_reg(addr_bits: u32) -> Register {
    match addr_bits {
        16 => Register::SI,
        32 => Register::ESI,
        _ => Register::RSI,
    }
}

fn string_dst_index_reg(addr_bits: u32) -> Register {
    match addr_bits {
        16 => Register::DI,
        32 => Register::EDI,
        _ => Register::RDI,
    }
}

fn instr_ins<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    addr_size_override: bool,
) -> Result<(), Exception> {
    require_iopl(state)?;
    let size = match instr.mnemonic() {
        Mnemonic::Insb => 1,
        Mnemonic::Insw => 2,
        Mnemonic::Insd => 4,
        _ => return Err(Exception::InvalidOpcode),
    };

    let port = state.read_reg(Register::DX) as u16;
    let df = state.get_flag(crate::state::RFLAGS_DF);
    let step: i64 = if df { -(size as i64) } else { size as i64 };

    let addr_size = effective_addr_size(state, addr_size_override)?;
    let count_reg = string_count_reg(addr_size);
    let index_reg = string_dst_index_reg(addr_size);
    let addr_mask = mask_bits(addr_size);

    let has_rep = instr.has_rep_prefix() || instr.has_repne_prefix();
    let mut count = if has_rep {
        state.read_reg(count_reg)
    } else {
        1
    };
    if has_rep && count == 0 {
        return Ok(());
    }
    let mut di = state.read_reg(index_reg) & addr_mask;
    let seg_base = state.seg_base_reg(Register::ES);

    while count != 0 {
        let val = bus.io_read(port, size)?;
        let addr = state.apply_a20(seg_base.wrapping_add(di & addr_mask));
        match size {
            1 => bus.write_u8(addr, val as u8)?,
            2 => write_u16_wrapped(state, bus, addr, val as u16)?,
            4 => write_u32_wrapped(state, bus, addr, val as u32)?,
            _ => unreachable!(),
        }
        di = (di as i64).wrapping_add(step) as u64;
        di &= addr_mask;
        if has_rep {
            count = count.wrapping_sub(1);
        } else {
            break;
        }
    }

    state.write_reg(index_reg, di);
    if has_rep {
        state.write_reg(count_reg, count);
    }
    Ok(())
}

fn instr_outs<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    addr_size_override: bool,
) -> Result<(), Exception> {
    require_iopl(state)?;
    let size = match instr.mnemonic() {
        Mnemonic::Outsb => 1,
        Mnemonic::Outsw => 2,
        Mnemonic::Outsd => 4,
        _ => return Err(Exception::InvalidOpcode),
    };

    let port = state.read_reg(Register::DX) as u16;
    let df = state.get_flag(crate::state::RFLAGS_DF);
    let step: i64 = if df { -(size as i64) } else { size as i64 };

    let addr_size = effective_addr_size(state, addr_size_override)?;
    let count_reg = string_count_reg(addr_size);
    let index_reg = string_src_index_reg(addr_size);
    let addr_mask = mask_bits(addr_size);

    let has_rep = instr.has_rep_prefix() || instr.has_repne_prefix();
    let mut count = if has_rep {
        state.read_reg(count_reg)
    } else {
        1
    };
    if has_rep && count == 0 {
        return Ok(());
    }
    let mut si = state.read_reg(index_reg) & addr_mask;
    let seg = instr.segment_prefix();
    let seg_reg = if seg == Register::None {
        Register::DS
    } else {
        seg
    };
    let seg_base = state.seg_base_reg(seg_reg);

    while count != 0 {
        let addr = state.apply_a20(seg_base.wrapping_add(si & addr_mask));
        let val: u64 = match size {
            1 => bus.read_u8(addr)? as u64,
            2 => read_u16_wrapped(state, bus, addr)? as u64,
            4 => read_u32_wrapped(state, bus, addr)? as u64,
            _ => unreachable!(),
        };
        bus.io_write(port, size, val)?;
        si = (si as i64).wrapping_add(step) as u64;
        si &= addr_mask;
        if has_rep {
            count = count.wrapping_sub(1);
        } else {
            break;
        }
    }

    state.write_reg(index_reg, si);
    if has_rep {
        state.write_reg(count_reg, count);
    }
    Ok(())
}

// -------------------------------------------------------------------------------------------------
// Stack helpers
// -------------------------------------------------------------------------------------------------

fn push_u16<B: CpuBus>(state: &mut CpuState, bus: &mut B, val: u16) -> Result<(), Exception> {
    push_sized(state, bus, val as u64, 2)
}

fn pop_u16<B: CpuBus>(state: &mut CpuState, bus: &mut B) -> Result<u16, Exception> {
    Ok(pop_sized(state, bus, 2)? as u16)
}

fn set_real_mode_seg(seg: &mut crate::state::Segment, selector: u16) {
    seg.selector = selector;
    seg.base = (selector as u64) << 4;
    seg.limit = 0xFFFF;
    seg.access = 0;
}

// -------------------------------------------------------------------------------------------------
// Privileged MOV / POP / segment loads
// -------------------------------------------------------------------------------------------------

fn instr_mov_privileged<B: CpuBus>(
    _ctx: &mut AssistContext,
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<(), Exception> {
    // Segment loads in protected/long mode.
    if instr.op_kind(0) == OpKind::Register && is_seg_reg(instr.op0_register()) {
        let seg = instr.op0_register();
        if seg == Register::CS {
            return Err(Exception::InvalidOpcode);
        }
        let selector = read_op_u16(state, bus, instr, 1, next_ip)?;
        let seg_enum = seg_from_reg(seg)?;
        let reason = if seg_enum == Seg::SS {
            LoadReason::Stack
        } else {
            LoadReason::Data
        };
        state.load_seg(bus, seg_enum, selector, reason)?;
        state.set_rip(next_ip);
        return Ok(());
    }

    // MOV to/from control/debug registers.
    if instr.op_kind(0) == OpKind::Register
        && (is_ctrl_reg(instr.op0_register()) || is_debug_reg(instr.op0_register()))
        || instr.op_kind(1) == OpKind::Register
            && (is_ctrl_reg(instr.op1_register()) || is_debug_reg(instr.op1_register()))
    {
        instr_mov_cr_dr(state, instr)?;
        state.set_rip(next_ip);
        return Ok(());
    }

    Err(Exception::InvalidOpcode)
}

fn instr_mov_cr_dr(state: &mut CpuState, instr: &Instruction) -> Result<(), Exception> {
    require_cpl0(state)?;
    let dst = instr.op0_register();
    let src = instr.op1_register();
    if is_ctrl_reg(dst) {
        // mov crX, reg
        let val = state.read_reg(src);
        match dst {
            Register::CR0 => state.control.cr0 = val,
            Register::CR2 => state.control.cr2 = val,
            Register::CR3 => state.control.cr3 = val,
            Register::CR4 => state.control.cr4 = val,
            Register::CR8 => state.control.cr8 = val,
            _ => return Err(Exception::InvalidOpcode),
        }
        state.update_mode();
        return Ok(());
    }
    if is_ctrl_reg(src) {
        // mov reg, crX
        let val = match src {
            Register::CR0 => state.control.cr0,
            Register::CR2 => state.control.cr2,
            Register::CR3 => state.control.cr3,
            Register::CR4 => state.control.cr4,
            Register::CR8 => state.control.cr8,
            _ => return Err(Exception::InvalidOpcode),
        };
        state.write_reg(dst, val);
        return Ok(());
    }

    if is_debug_reg(dst) {
        let val = state.read_reg(src);
        match dst {
            Register::DR0 => state.debug.dr[0] = val,
            Register::DR1 => state.debug.dr[1] = val,
            Register::DR2 => state.debug.dr[2] = val,
            Register::DR3 => state.debug.dr[3] = val,
            Register::DR6 => state.debug.dr6 = val,
            Register::DR7 => state.debug.dr7 = val,
            _ => return Err(Exception::InvalidOpcode),
        }
        return Ok(());
    }
    if is_debug_reg(src) {
        let val = match src {
            Register::DR0 => state.debug.dr[0],
            Register::DR1 => state.debug.dr[1],
            Register::DR2 => state.debug.dr[2],
            Register::DR3 => state.debug.dr[3],
            Register::DR6 => state.debug.dr6,
            Register::DR7 => state.debug.dr7,
            _ => return Err(Exception::InvalidOpcode),
        };
        state.write_reg(dst, val);
        return Ok(());
    }

    Err(Exception::InvalidOpcode)
}

fn instr_pop_privileged<B: CpuBus>(
    _ctx: &mut AssistContext,
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
) -> Result<(), Exception> {
    if instr.op_kind(0) != OpKind::Register || !is_seg_reg(instr.op0_register()) {
        return Err(Exception::InvalidOpcode);
    }
    let seg = instr.op0_register();
    if seg == Register::CS {
        return Err(Exception::InvalidOpcode);
    }
    // POP segment always pops a 16-bit selector, even in 32-bit mode.
    let selector = pop_u16(state, bus)?;
    let seg_enum = seg_from_reg(seg)?;
    let reason = if seg_enum == Seg::SS {
        LoadReason::Stack
    } else {
        LoadReason::Data
    };
    state.load_seg(bus, seg_enum, selector, reason)
}

// -------------------------------------------------------------------------------------------------
// Far control transfers (real + protected mode)
// -------------------------------------------------------------------------------------------------

fn instr_jmp_far<B: CpuBus>(
    _ctx: &mut AssistContext,
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<(), Exception> {
    match instr.op_kind(0) {
        OpKind::FarBranch16 => {
            let sel = instr.far_branch_selector();
            let off = instr.far_branch16();
            far_jump(state, bus, sel, off as u64)
        }
        OpKind::FarBranch32 => {
            let sel = instr.far_branch_selector();
            let off = instr.far_branch32();
            far_jump(state, bus, sel, off as u64)
        }
        _ => {
            // Near JMP should have been handled by Tier-0 already.
            state.set_rip(next_ip);
            Ok(())
        }
    }
}

fn instr_call_far<B: CpuBus>(
    _ctx: &mut AssistContext,
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<(), Exception> {
    match instr.op_kind(0) {
        OpKind::FarBranch16 => {
            let sel = instr.far_branch_selector();
            let off = instr.far_branch16() as u64;
            far_call(state, bus, sel, off, next_ip)
        }
        OpKind::FarBranch32 => {
            let sel = instr.far_branch_selector();
            let off = instr.far_branch32() as u64;
            far_call(state, bus, sel, off, next_ip)
        }
        _ => Err(Exception::InvalidOpcode),
    }
}

fn instr_retf<B: CpuBus>(
    _ctx: &mut AssistContext,
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
) -> Result<(), Exception> {
    let pop_imm = if instr.op_count() == 1 && instr.op_kind(0) == OpKind::Immediate16 {
        instr.immediate16() as u32
    } else {
        0
    };

    let off_bits = state.bitness();
    let off_size = off_bits / 8;
    let off = pop_sized(state, bus, off_size)? & mask_bits(off_bits);
    let cs = pop_u16(state, bus)?;
    let sp = state.stack_ptr().wrapping_add(pop_imm as u64);
    state.set_stack_ptr(sp);
    far_jump(state, bus, cs, off)
}

fn push_sized<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    val: u64,
    size: u32,
) -> Result<(), Exception> {
    let sp_bits = state.stack_ptr_bits();
    let mut sp = state.stack_ptr();
    sp = sp.wrapping_sub(size as u64) & mask_bits(sp_bits);
    state.set_stack_ptr(sp);
    let addr = state.apply_a20(state.seg_base_reg(Register::SS).wrapping_add(sp));
    write_mem(state, bus, addr, size * 8, val)
}

fn pop_sized<B: CpuBus>(state: &mut CpuState, bus: &mut B, size: u32) -> Result<u64, Exception> {
    let sp_bits = state.stack_ptr_bits();
    let sp = state.stack_ptr();
    let addr = state.apply_a20(state.seg_base_reg(Register::SS).wrapping_add(sp));
    let v = read_mem(state, bus, addr, size * 8)?;
    let next = sp.wrapping_add(size as u64) & mask_bits(sp_bits);
    state.set_stack_ptr(next);
    Ok(v)
}

fn far_jump<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    selector: u16,
    offset: u64,
) -> Result<(), Exception> {
    match state.mode {
        CpuMode::Real | CpuMode::Vm86 => {
            set_real_mode_seg(&mut state.segments.cs, selector);
            state.set_rip(offset);
            Ok(())
        }
        CpuMode::Protected | CpuMode::Long => {
            state.load_seg(bus, Seg::CS, selector, LoadReason::FarControlTransfer)?;
            state.set_rip(offset);
            Ok(())
        }
    }
}

fn far_call<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    selector: u16,
    offset: u64,
    return_ip: u64,
) -> Result<(), Exception> {
    // Push current CS then return IP.
    let cs = state.segments.cs.selector;
    push_u16(state, bus, cs)?;
    let ret_size = state.bitness() / 8;
    push_sized(state, bus, return_ip, ret_size)?;
    far_jump(state, bus, selector, offset)
}

// -------------------------------------------------------------------------------------------------
// Descriptor table ops
// -------------------------------------------------------------------------------------------------

fn instr_lgdt_lidt<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<(), Exception> {
    require_cpl0(state)?;
    if instr.op_kind(0) != OpKind::Memory {
        return Err(Exception::InvalidOpcode);
    }
    let addr = calc_ea(state, instr, next_ip, true)?;
    let limit = read_u16_wrapped(state, bus, addr)?;
    let base = match state.bitness() {
        64 => read_u64_wrapped(state, bus, addr.wrapping_add(2))?,
        16 => (read_u32_wrapped(state, bus, addr.wrapping_add(2))? & 0x00FF_FFFF) as u64,
        _ => read_u32_wrapped(state, bus, addr.wrapping_add(2))? as u64,
    };
    match instr.mnemonic() {
        Mnemonic::Lgdt => {
            state.tables.gdtr.limit = limit;
            state.tables.gdtr.base = base;
        }
        Mnemonic::Lidt => {
            state.tables.idtr.limit = limit;
            state.tables.idtr.base = base;
        }
        _ => return Err(Exception::InvalidOpcode),
    }
    Ok(())
}

fn instr_sgdt_sidt<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<(), Exception> {
    if instr.op_kind(0) != OpKind::Memory {
        return Err(Exception::InvalidOpcode);
    }
    let addr = calc_ea(state, instr, next_ip, true)?;
    let (limit, base) = match instr.mnemonic() {
        Mnemonic::Sgdt => (state.tables.gdtr.limit, state.tables.gdtr.base),
        Mnemonic::Sidt => (state.tables.idtr.limit, state.tables.idtr.base),
        _ => return Err(Exception::InvalidOpcode),
    };
    write_u16_wrapped(state, bus, addr, limit)?;
    match state.bitness() {
        64 => write_u64_wrapped(state, bus, addr.wrapping_add(2), base)?,
        16 => write_u32_wrapped(state, bus, addr.wrapping_add(2), base as u32 & 0x00FF_FFFF)?,
        _ => write_u32_wrapped(state, bus, addr.wrapping_add(2), base as u32)?,
    }
    Ok(())
}

fn instr_ltr_lldt<B: CpuBus>(
    _ctx: &mut AssistContext,
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<(), Exception> {
    require_cpl0(state)?;
    let selector = read_op_u16(state, bus, instr, 0, next_ip)?;
    match instr.mnemonic() {
        Mnemonic::Ltr => state.load_tr(bus, selector),
        Mnemonic::Lldt => state.load_ldtr(bus, selector),
        _ => Err(Exception::InvalidOpcode),
    }
}

fn instr_str_sldt<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<(), Exception> {
    let selector = match instr.mnemonic() {
        Mnemonic::Str => state.tables.tr.selector,
        Mnemonic::Sldt => state.tables.ldtr.selector,
        _ => return Err(Exception::InvalidOpcode),
    };
    write_op_u16(state, bus, instr, 0, selector, next_ip)
}

fn instr_lmsw_smsw<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<(), Exception> {
    match instr.mnemonic() {
        Mnemonic::Lmsw => {
            require_cpl0(state)?;
            let msw = read_op_u16(state, bus, instr, 0, next_ip)? as u64;
            let old = state.control.cr0;
            let mut next = (old & !0xF) | (msw & 0xF);
            // LMSW cannot clear PE once set.
            if (old & crate::state::CR0_PE) != 0 {
                next |= crate::state::CR0_PE;
            }
            state.control.cr0 = next;
            state.update_mode();
            Ok(())
        }
        Mnemonic::Smsw => {
            let msw = (state.control.cr0 & 0xFFFF) as u16;
            match instr.op_kind(0) {
                OpKind::Register => {
                    state.write_reg(instr.op0_register(), msw as u64);
                    Ok(())
                }
                OpKind::Memory => {
                    let addr = calc_ea(state, instr, next_ip, true)?;
                    write_u16_wrapped(state, bus, addr, msw)
                }
                _ => Err(Exception::InvalidOpcode),
            }
        }
        _ => Err(Exception::InvalidOpcode),
    }
}

fn instr_invlpg(
    ctx: &mut AssistContext,
    state: &CpuState,
    bus: &mut impl CpuBus,
    instr: &Instruction,
    next_ip: u64,
) -> Result<(), Exception> {
    require_cpl0(state)?;
    if instr.op_kind(0) != OpKind::Memory {
        return Err(Exception::InvalidOpcode);
    }
    // INVLPG takes a linear address operand. In non-long modes, linear addresses
    // are 32-bit and wrap around on overflow of `segment_base + offset`.
    let mut addr = calc_ea(state, instr, next_ip, true)?;
    if state.mode != CpuMode::Long {
        addr &= 0xffff_ffff;
    }
    bus.invlpg(addr);
    ctx.record_invlpg(addr);
    Ok(())
}

// -------------------------------------------------------------------------------------------------
// SYSCALL/SYSRET/SYSENTER/SYSEXIT/SWAPGS
// -------------------------------------------------------------------------------------------------

fn is_canonical(addr: u64, bits: u8) -> bool {
    if bits >= 64 {
        return true;
    }
    let sign_bit = 1u64 << (bits - 1);
    let mask = (!0u64) << bits;
    if (addr & sign_bit) == 0 {
        (addr & mask) == 0
    } else {
        (addr & mask) == mask
    }
}

fn instr_syscall(
    ctx: &AssistContext,
    state: &mut CpuState,
    return_ip: u64,
) -> Result<(), Exception> {
    if state.mode != CpuMode::Long {
        return Err(Exception::InvalidOpcode);
    }
    if (state.msr.efer & msr::EFER_SCE) == 0 {
        return Err(Exception::InvalidOpcode);
    }

    state.write_reg(Register::RCX, return_ip);
    state.write_reg(Register::R11, state.rflags());

    let star = state.msr.star;
    let syscall_cs = ((star >> 32) & 0xFFFF) as u16;
    state.segments.cs.selector = syscall_cs & !0b11;
    state.segments.ss.selector = syscall_cs.wrapping_add(8) & !0b11;

    let fmask = state.msr.fmask;
    state.set_rflags(state.rflags() & !fmask);

    let target = state.msr.lstar;
    if !is_canonical(target, ctx.features.linear_address_bits) {
        return Err(Exception::gp0());
    }
    state.set_rip(target);
    Ok(())
}

fn instr_sysret(ctx: &AssistContext, state: &mut CpuState) -> Result<(), Exception> {
    require_cpl0(state)?;
    if state.mode != CpuMode::Long {
        return Err(Exception::InvalidOpcode);
    }
    if (state.msr.efer & msr::EFER_SCE) == 0 {
        return Err(Exception::InvalidOpcode);
    }

    let target = state.read_reg(Register::RCX);
    if !is_canonical(target, ctx.features.linear_address_bits) {
        return Err(Exception::gp0());
    }

    state.set_rflags(state.read_reg(Register::R11));

    let star = state.msr.star;
    let base = ((star >> 48) & 0xFFFF) as u16;
    let user_cs = base.wrapping_add(16);
    let user_ss = base.wrapping_add(8);
    state.segments.cs.selector = (user_cs & !0b11) | 0b11;
    state.segments.ss.selector = (user_ss & !0b11) | 0b11;

    state.set_rip(target);
    Ok(())
}

fn instr_sysenter(state: &mut CpuState) -> Result<(), Exception> {
    if state.mode == CpuMode::Real || state.mode == CpuMode::Vm86 {
        return Err(Exception::InvalidOpcode);
    }

    let cs = state.msr.sysenter_cs as u16;
    if cs == 0 {
        return Err(Exception::gp0());
    }
    state.segments.cs.selector = cs & !0b11;
    state.segments.ss.selector = state.segments.cs.selector.wrapping_add(8) & !0b11;

    match state.mode {
        CpuMode::Protected => {
            state.write_reg(Register::ESP, state.msr.sysenter_esp);
            state.set_rip(state.msr.sysenter_eip);
        }
        CpuMode::Long => {
            state.write_reg(Register::RSP, state.msr.sysenter_esp);
            state.set_rip(state.msr.sysenter_eip);
        }
        _ => {}
    }
    Ok(())
}

fn instr_sysexit(state: &mut CpuState) -> Result<(), Exception> {
    require_cpl0(state)?;
    if state.mode == CpuMode::Real || state.mode == CpuMode::Vm86 {
        return Err(Exception::InvalidOpcode);
    }

    let cs = state.msr.sysenter_cs as u16;
    if cs == 0 {
        return Err(Exception::gp0());
    }
    let cs_base = cs & !0b11;
    state.segments.cs.selector = cs_base.wrapping_add(16) | 0b11;
    state.segments.ss.selector = cs_base.wrapping_add(24) | 0b11;

    match state.mode {
        CpuMode::Protected => {
            let new_rip = state.read_reg(Register::EDX) as u32 as u64;
            let new_rsp = state.read_reg(Register::ECX) as u32 as u64;
            state.write_reg(Register::ESP, new_rsp);
            state.set_rip(new_rip);
        }
        CpuMode::Long => {
            let new_rip = state.read_reg(Register::RCX);
            let new_rsp = state.read_reg(Register::RDX);
            state.write_reg(Register::RSP, new_rsp);
            state.set_rip(new_rip);
        }
        _ => {}
    }
    Ok(())
}

fn instr_swapgs(state: &mut CpuState) -> Result<(), Exception> {
    require_cpl0(state)?;
    if state.mode != CpuMode::Long {
        return Err(Exception::InvalidOpcode);
    }
    core::mem::swap(&mut state.msr.gs_base, &mut state.msr.kernel_gs_base);
    state.segments.gs.base = state.msr.gs_base;
    Ok(())
}
