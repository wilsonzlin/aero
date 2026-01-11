use aero_x86::{DecodedInst, Instruction, Mnemonic, OpKind, Register};

use crate::cpuid::{self, CpuFeatures};
use crate::exception::{AssistReason, Exception};
use crate::mem::CpuBus;
use crate::msr;
use crate::state::{mask_bits, CpuMode, CpuState, RFLAGS_IF, RFLAGS_IOPL_MASK, RFLAGS_TF};

/// Runtime context needed by the Tier-0 assist layer.
///
/// Tier-0 is intentionally minimal and does not model system state like MSRs,
/// descriptor tables, or deterministic time. When it encounters an instruction
/// that depends on that state, it exits with [`AssistReason`]. The caller feeds
/// those exits into [`handle_assist`], which emulates the instruction against
/// the JIT ABI [`CpuState`].
#[derive(Debug, Clone)]
pub struct AssistContext {
    /// CPUID feature policy used for `CPUID` and for masking MSR writes (e.g.
    /// keeping `IA32_EFER` coherent with advertised features).
    pub features: CpuFeatures,
    /// Deterministic increment applied to `state.msr.tsc` after each `RDTSC` /
    /// `RDTSCP`.
    pub tsc_step: u64,
    /// Backing storage for `IA32_TSC_AUX` (returned in `RDTSCP` ECX).
    pub tsc_aux: u32,
    /// Optional log of `INVLPG` linear addresses (useful for integration tests).
    pub invlpg_log: Vec<u64>,
}

impl Default for AssistContext {
    fn default() -> Self {
        Self {
            features: CpuFeatures::default(),
            tsc_step: 1,
            tsc_aux: 0,
            invlpg_log: Vec::new(),
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
    state: &mut CpuState,
    bus: &mut B,
    _reason: AssistReason,
) -> Result<(), Exception> {
    let ip = state.rip();
    let fetch_addr = state.seg_base_reg(Register::CS).wrapping_add(ip);
    let bytes = bus.fetch(fetch_addr, 15)?;
    let decoded =
        aero_x86::decode(&bytes, ip, state.bitness()).map_err(|_| Exception::InvalidOpcode)?;

    exec_decoded(ctx, state, bus, &decoded)
}

fn exec_decoded<B: CpuBus>(
    ctx: &mut AssistContext,
    state: &mut CpuState,
    bus: &mut B,
    decoded: &DecodedInst,
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
            instr_rdmsr(ctx, state)?;
            state.set_rip(next_ip_raw);
            Ok(())
        }
        Mnemonic::Wrmsr => {
            instr_wrmsr(ctx, state)?;
            state.set_rip(next_ip_raw);
            Ok(())
        }
        Mnemonic::Rdtsc => {
            instr_rdtsc(ctx, state);
            state.set_rip(next_ip_raw);
            Ok(())
        }
        Mnemonic::Rdtscp => {
            instr_rdtscp(ctx, state);
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
            instr_ins(state, bus, instr)?;
            state.set_rip(next_ip_raw);
            Ok(())
        }
        Mnemonic::Outsb | Mnemonic::Outsw | Mnemonic::Outsd => {
            instr_outs(state, bus, instr)?;
            state.set_rip(next_ip_raw);
            Ok(())
        }
        Mnemonic::Cli => {
            instr_cli(state)?;
            state.set_rip(next_ip_raw);
            Ok(())
        }
        Mnemonic::Sti => {
            instr_sti(state)?;
            state.set_rip(next_ip_raw);
            Ok(())
        }
        Mnemonic::Int | Mnemonic::Int1 | Mnemonic::Int3 | Mnemonic::Into => {
            instr_int(state, bus, instr, next_ip_raw)?;
            Ok(())
        }
        Mnemonic::Iret | Mnemonic::Iretd | Mnemonic::Iretq => {
            instr_iret(state, bus, instr)?;
            Ok(())
        }
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
            instr_invlpg(ctx, state, instr, next_ip_raw)?;
            state.set_rip(next_ip_raw);
            Ok(())
        }
        Mnemonic::Swapgs => {
            instr_swapgs(state)?;
            state.set_rip(next_ip_raw);
            Ok(())
        }
        Mnemonic::Syscall => {
            instr_syscall(state, next_ip_raw)?;
            Ok(())
        }
        Mnemonic::Sysret => {
            instr_sysret(state)?;
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

fn is_ctrl_reg(reg: Register) -> bool {
    matches!(
        reg,
        Register::CR0 | Register::CR2 | Register::CR3 | Register::CR4 | Register::CR8
    )
}

fn is_debug_reg(reg: Register) -> bool {
    matches!(
        reg,
        Register::DR0 | Register::DR1 | Register::DR2 | Register::DR3 | Register::DR6 | Register::DR7
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

fn calc_ea(state: &CpuState, instr: &Instruction, next_ip: u64, include_seg: bool) -> Result<u64, Exception> {
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
        let base_val = if base == Register::RIP { next_ip } else { state.read_reg(base) };
        offset += (base_val & mask_bits(addr_bits)) as i128;
    }
    if index != Register::None {
        let idx_val = state.read_reg(index) & mask_bits(addr_bits);
        offset += (idx_val as i128) * (scale as i128);
    }

    let addr = (offset as u64) & mask_bits(addr_bits);
    if include_seg {
        Ok(state
            .seg_base_reg(instr.memory_segment())
            .wrapping_add(addr))
    } else {
        Ok(addr)
    }
}

fn read_mem<B: CpuBus>(bus: &mut B, addr: u64, bits: u32) -> Result<u64, Exception> {
    match bits {
        8 => Ok(bus.read_u8(addr)? as u64),
        16 => Ok(bus.read_u16(addr)? as u64),
        32 => Ok(bus.read_u32(addr)? as u64),
        64 => Ok(bus.read_u64(addr)?),
        _ => Err(Exception::InvalidOpcode),
    }
}

fn write_mem<B: CpuBus>(bus: &mut B, addr: u64, bits: u32, val: u64) -> Result<(), Exception> {
    match bits {
        8 => bus.write_u8(addr, val as u8),
        16 => bus.write_u16(addr, val as u16),
        32 => bus.write_u32(addr, val as u32),
        64 => bus.write_u64(addr, val),
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
            Ok(bus.read_u16(addr)?)
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
            bus.write_u16(addr, value)
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

fn msr_read(ctx: &AssistContext, state: &CpuState, msr_index: u32) -> Result<u64, Exception> {
    match msr_index {
        msr::IA32_EFER => Ok(state.msr.efer),
        msr::IA32_STAR => Ok(state.msr.star),
        msr::IA32_LSTAR => Ok(state.msr.lstar),
        msr::IA32_CSTAR => Ok(state.msr.cstar),
        msr::IA32_FMASK => Ok(state.msr.fmask),
        msr::IA32_SYSENTER_CS => Ok(state.msr.sysenter_cs),
        msr::IA32_SYSENTER_EIP => Ok(state.msr.sysenter_eip),
        msr::IA32_SYSENTER_ESP => Ok(state.msr.sysenter_esp),
        msr::IA32_FS_BASE => Ok(state.msr.fs_base),
        msr::IA32_GS_BASE => Ok(state.msr.gs_base),
        msr::IA32_KERNEL_GS_BASE => Ok(state.msr.kernel_gs_base),
        msr::IA32_APIC_BASE => Ok(state.msr.apic_base),
        msr::IA32_TSC => Ok(state.msr.tsc),
        msr::IA32_TSC_AUX => Ok(ctx.tsc_aux as u64),
        _ => Err(Exception::gp0()),
    }
}

fn msr_write(ctx: &mut AssistContext, state: &mut CpuState, msr_index: u32, value: u64) -> Result<(), Exception> {
    match msr_index {
        msr::IA32_EFER => {
            // Mirror `msr::MsrState` write semantics: keep CPUID/MSR coherent and
            // preserve the read-only LMA bit.
            let mut next = value;
            next = (next & !msr::EFER_LMA) | (state.msr.efer & msr::EFER_LMA);

            if (ctx.features.ext1_edx & cpuid::bits::EXT1_EDX_SYSCALL) == 0 {
                next &= !msr::EFER_SCE;
            }
            if (ctx.features.ext1_edx & cpuid::bits::EXT1_EDX_LM) == 0 {
                next &= !msr::EFER_LME;
            }
            if (ctx.features.ext1_edx & cpuid::bits::EXT1_EDX_NX) == 0 {
                next &= !msr::EFER_NXE;
            }

            state.msr.efer = next;
            state.update_mode();
            Ok(())
        }
        msr::IA32_STAR => {
            state.msr.star = value;
            Ok(())
        }
        msr::IA32_LSTAR => {
            state.msr.lstar = value;
            Ok(())
        }
        msr::IA32_CSTAR => {
            state.msr.cstar = value;
            Ok(())
        }
        msr::IA32_FMASK => {
            state.msr.fmask = value;
            Ok(())
        }
        msr::IA32_SYSENTER_CS => {
            state.msr.sysenter_cs = value;
            Ok(())
        }
        msr::IA32_SYSENTER_EIP => {
            state.msr.sysenter_eip = value;
            Ok(())
        }
        msr::IA32_SYSENTER_ESP => {
            state.msr.sysenter_esp = value;
            Ok(())
        }
        msr::IA32_FS_BASE => {
            state.msr.fs_base = value;
            if state.mode == CpuMode::Long {
                state.segments.fs.base = value;
            }
            Ok(())
        }
        msr::IA32_GS_BASE => {
            state.msr.gs_base = value;
            if state.mode == CpuMode::Long {
                state.segments.gs.base = value;
            }
            Ok(())
        }
        msr::IA32_KERNEL_GS_BASE => {
            state.msr.kernel_gs_base = value;
            Ok(())
        }
        msr::IA32_APIC_BASE => {
            state.msr.apic_base = value;
            Ok(())
        }
        msr::IA32_TSC => {
            state.msr.tsc = value;
            Ok(())
        }
        msr::IA32_TSC_AUX => {
            ctx.tsc_aux = value as u32;
            Ok(())
        }
        _ => Err(Exception::gp0()),
    }
}

fn instr_rdmsr(ctx: &AssistContext, state: &mut CpuState) -> Result<(), Exception> {
    require_cpl0(state)?;
    let msr_index = state.read_reg(Register::ECX) as u32;
    let value = msr_read(ctx, state, msr_index)?;
    state.write_reg(Register::EAX, value as u32 as u64);
    state.write_reg(Register::EDX, (value >> 32) as u32 as u64);
    Ok(())
}

fn instr_wrmsr(ctx: &mut AssistContext, state: &mut CpuState) -> Result<(), Exception> {
    require_cpl0(state)?;
    let msr_index = state.read_reg(Register::ECX) as u32;
    let eax = state.read_reg(Register::EAX) as u32 as u64;
    let edx = state.read_reg(Register::EDX) as u32 as u64;
    let value = (edx << 32) | eax;
    msr_write(ctx, state, msr_index, value)?;
    Ok(())
}

// -------------------------------------------------------------------------------------------------
// Time
// -------------------------------------------------------------------------------------------------

fn instr_rdtsc(ctx: &AssistContext, state: &mut CpuState) {
    let tsc = state.msr.tsc;
    state.write_reg(Register::EAX, tsc as u32 as u64);
    state.write_reg(Register::EDX, (tsc >> 32) as u32 as u64);
    state.msr.tsc = state.msr.tsc.wrapping_add(ctx.tsc_step);
}

fn instr_rdtscp(ctx: &AssistContext, state: &mut CpuState) {
    let tsc = state.msr.tsc;
    state.write_reg(Register::EAX, tsc as u32 as u64);
    state.write_reg(Register::EDX, (tsc >> 32) as u32 as u64);
    state.write_reg(Register::ECX, ctx.tsc_aux as u64);
    state.msr.tsc = state.msr.tsc.wrapping_add(ctx.tsc_step);
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

fn rep_count_reg(state: &CpuState) -> Register {
    match state.bitness() {
        16 => Register::CX,
        32 => Register::ECX,
        _ => Register::RCX,
    }
}

fn index_reg_for_mode(state: &CpuState, is_dest: bool) -> Register {
    match state.bitness() {
        16 => {
            if is_dest {
                Register::DI
            } else {
                Register::SI
            }
        }
        32 => {
            if is_dest {
                Register::EDI
            } else {
                Register::ESI
            }
        }
        _ => {
            if is_dest {
                Register::RDI
            } else {
                Register::RSI
            }
        }
    }
}

fn instr_ins<B: CpuBus>(state: &mut CpuState, bus: &mut B, instr: &Instruction) -> Result<(), Exception> {
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

    let count_reg = rep_count_reg(state);
    let mut count = if instr.has_rep_prefix() { state.read_reg(count_reg) } else { 1 };
    let mut di = state.read_reg(index_reg_for_mode(state, true));
    let addr_mask = mask_bits(state.bitness());

    while count != 0 {
        let val = bus.io_read(port, size)?;
        let addr = state.seg_base_reg(Register::ES).wrapping_add(di & addr_mask);
        match size {
            1 => bus.write_u8(addr, val as u8)?,
            2 => bus.write_u16(addr, val as u16)?,
            4 => bus.write_u32(addr, val as u32)?,
            _ => unreachable!(),
        }
        di = (di as i64).wrapping_add(step) as u64;
        di &= addr_mask;
        if instr.has_rep_prefix() {
            count = count.wrapping_sub(1);
        } else {
            break;
        }
    }

    state.write_reg(index_reg_for_mode(state, true), di);
    if instr.has_rep_prefix() {
        state.write_reg(count_reg, count);
    }
    Ok(())
}

fn instr_outs<B: CpuBus>(state: &mut CpuState, bus: &mut B, instr: &Instruction) -> Result<(), Exception> {
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

    let count_reg = rep_count_reg(state);
    let mut count = if instr.has_rep_prefix() { state.read_reg(count_reg) } else { 1 };
    let mut si = state.read_reg(index_reg_for_mode(state, false));
    let addr_mask = mask_bits(state.bitness());

    while count != 0 {
        let addr = state.seg_base_reg(Register::DS).wrapping_add(si & addr_mask);
        let val: u64 = match size {
            1 => bus.read_u8(addr)? as u64,
            2 => bus.read_u16(addr)? as u64,
            4 => bus.read_u32(addr)? as u64,
            _ => unreachable!(),
        };
        bus.io_write(port, size, val)?;
        si = (si as i64).wrapping_add(step) as u64;
        si &= addr_mask;
        if instr.has_rep_prefix() {
            count = count.wrapping_sub(1);
        } else {
            break;
        }
    }

    state.write_reg(index_reg_for_mode(state, false), si);
    if instr.has_rep_prefix() {
        state.write_reg(count_reg, count);
    }
    Ok(())
}

// -------------------------------------------------------------------------------------------------
// Interrupt flag manipulation + real-mode INT/IRET
// -------------------------------------------------------------------------------------------------

fn instr_cli(state: &mut CpuState) -> Result<(), Exception> {
    require_iopl(state)?;
    state.set_flag(RFLAGS_IF, false);
    Ok(())
}

fn instr_sti(state: &mut CpuState) -> Result<(), Exception> {
    require_iopl(state)?;
    state.set_flag(RFLAGS_IF, true);
    Ok(())
}

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

fn instr_int<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    return_ip: u64,
) -> Result<(), Exception> {
    if state.mode != CpuMode::Real && state.mode != CpuMode::Vm86 {
        return Err(Exception::Unimplemented("INT in protected/long mode"));
    }

    let vector: u8 = match instr.mnemonic() {
        Mnemonic::Int => instr.immediate8(),
        Mnemonic::Int3 => 3,
        Mnemonic::Int1 => 1,
        Mnemonic::Into => {
            if !state.get_flag(crate::state::RFLAGS_OF) {
                state.set_rip(return_ip);
                return Ok(());
            }
            4
        }
        _ => return Err(Exception::InvalidOpcode),
    };

    let flags = state.rflags() as u16;
    let cs = state.segments.cs.selector;
    let ip = (return_ip & 0xFFFF) as u16;

    push_u16(state, bus, flags)?;
    push_u16(state, bus, cs)?;
    push_u16(state, bus, ip)?;

    // Clear IF + TF (interrupt gate behavior).
    state.set_flag(RFLAGS_IF, false);
    state.set_flag(RFLAGS_TF, false);

    let ivt = (vector as u64) * 4;
    let new_ip = bus.read_u16(ivt)?;
    let new_cs = bus.read_u16(ivt + 2)?;

    set_real_mode_seg(&mut state.segments.cs, new_cs);
    state.set_rip(new_ip as u64);
    Ok(())
}

fn instr_iret<B: CpuBus>(state: &mut CpuState, bus: &mut B, _instr: &Instruction) -> Result<(), Exception> {
    if state.mode != CpuMode::Real && state.mode != CpuMode::Vm86 {
        return Err(Exception::Unimplemented("IRET in protected/long mode"));
    }

    let ip = pop_u16(state, bus)?;
    let cs = pop_u16(state, bus)?;
    let flags = pop_u16(state, bus)?;

    set_real_mode_seg(&mut state.segments.cs, cs);
    state.set_rip(ip as u64);
    // IRET restores FLAGS (16-bit in real mode).
    let new_flags = (state.rflags() & !0xFFFF) | (flags as u64);
    state.set_rflags(new_flags);
    Ok(())
}

// -------------------------------------------------------------------------------------------------
// Descriptor helpers (protected/long mode segment loads)
// -------------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct ParsedDesc {
    base: u64,
    limit: u32,
    typ: u8,
    s: bool,
    dpl: u8,
    present: bool,
    avl: bool,
    l: bool,
    db: bool,
    g: bool,
}

fn parse_descriptor_low(raw: u64) -> ParsedDesc {
    let limit_low = (raw & 0xFFFF) as u32;
    let base_low = ((raw >> 16) & 0xFFFF) as u32;
    let base_mid = ((raw >> 32) & 0xFF) as u32;
    let access = ((raw >> 40) & 0xFF) as u8;
    let limit_high = ((raw >> 48) & 0xF) as u32;
    let flags = ((raw >> 52) & 0xF) as u8;
    let base_high = ((raw >> 56) & 0xFF) as u32;

    let mut limit = limit_low | (limit_high << 16);
    let base32 = base_low | (base_mid << 16) | (base_high << 24);

    let avl = (flags & 0b0001) != 0;
    let l = (flags & 0b0010) != 0;
    let db = (flags & 0b0100) != 0;
    let g = (flags & 0b1000) != 0;
    if g {
        limit = (limit << 12) | 0xFFF;
    }

    ParsedDesc {
        base: base32 as u64,
        limit,
        typ: access & 0xF,
        s: (access & 0x10) != 0,
        dpl: (access >> 5) & 0b11,
        present: (access & 0x80) != 0,
        avl,
        l,
        db,
        g,
    }
}

fn parse_system_descriptor(raw_low: u64, raw_high: u64) -> ParsedDesc {
    let mut desc = parse_descriptor_low(raw_low);
    let base_high = (raw_high & 0xFFFF_FFFF) as u64;
    desc.base |= base_high << 32;
    desc
}

fn table_for_selector(state: &CpuState, selector: u16) -> Result<(u64, u32), Exception> {
    let ti = (selector & 0b100) != 0;
    if ti {
        if state.tables.ldtr.is_unusable() {
            return Err(Exception::gp(selector));
        }
        Ok((state.tables.ldtr.base, state.tables.ldtr.limit))
    } else {
        Ok((state.tables.gdtr.base, state.tables.gdtr.limit as u32))
    }
}

fn load_segment_descriptor<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    seg: Register,
    selector: u16,
    is_stack: bool,
) -> Result<(), Exception> {
    if selector == 0 {
        if is_stack {
            return Err(Exception::gp0());
        }
        let s = match seg {
            Register::ES => &mut state.segments.es,
            Register::DS => &mut state.segments.ds,
            Register::FS => &mut state.segments.fs,
            Register::GS => &mut state.segments.gs,
            _ => return Err(Exception::InvalidOpcode),
        };
        s.selector = 0;
        s.base = 0;
        s.limit = 0;
        s.access = crate::state::SEG_ACCESS_UNUSABLE;
        return Ok(());
    }

    let (table_base, table_limit) = table_for_selector(state, selector)?;
    let index = (selector >> 3) as u64;
    let byte_off = index * 8;
    if byte_off + 7 > table_limit as u64 {
        return Err(Exception::gp(selector));
    }
    let raw = bus.read_u64(table_base + byte_off)?;
    let desc = parse_descriptor_low(raw);
    if !desc.s {
        return Err(Exception::gp(selector));
    }
    if !desc.present {
        return if is_stack {
            Err(Exception::ss(selector))
        } else {
            Err(Exception::np(selector))
        };
    }

    let access = (desc.typ as u32)
        | ((desc.s as u32) << 4)
        | ((desc.dpl as u32) << 5)
        | ((desc.present as u32) << 7)
        | ((desc.avl as u32) << 8)
        | ((desc.l as u32) << 9)
        | ((desc.db as u32) << 10)
        | ((desc.g as u32) << 11);

    let seg_ref = match seg {
        Register::ES => &mut state.segments.es,
        Register::CS => &mut state.segments.cs,
        Register::SS => &mut state.segments.ss,
        Register::DS => &mut state.segments.ds,
        Register::FS => &mut state.segments.fs,
        Register::GS => &mut state.segments.gs,
        _ => return Err(Exception::InvalidOpcode),
    };

    seg_ref.selector = selector;
    seg_ref.base = desc.base;
    seg_ref.limit = desc.limit;
    seg_ref.access = access;

    if state.mode == CpuMode::Long {
        // In long mode FS/GS base comes from MSRs.
        if seg == Register::FS {
            seg_ref.base = state.msr.fs_base;
        } else if seg == Register::GS {
            seg_ref.base = state.msr.gs_base;
        }
    }

    if seg == Register::CS {
        state.update_mode();
    }

    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SystemSeg {
    Ldtr,
    Tr,
}

fn load_system_segment<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    seg: SystemSeg,
    selector: u16,
) -> Result<(), Exception> {
    require_cpl0(state)?;
    if selector == 0 {
        if seg == SystemSeg::Tr {
            return Err(Exception::gp0());
        }
        let dst = match seg {
            SystemSeg::Ldtr => &mut state.tables.ldtr,
            SystemSeg::Tr => &mut state.tables.tr,
        };
        dst.selector = 0;
        dst.base = 0;
        dst.limit = 0;
        dst.access = crate::state::SEG_ACCESS_UNUSABLE;
        return Ok(());
    }

    let (table_base, table_limit) = table_for_selector(state, selector)?;
    let index = (selector >> 3) as u64;
    let byte_off = index * 8;

    let long_mode = state.mode == CpuMode::Long;
    let entry_size = if long_mode { 16 } else { 8 };
    if byte_off + (entry_size as u64) - 1 > table_limit as u64 {
        return Err(Exception::gp(selector));
    }

    let raw_low = bus.read_u64(table_base + byte_off)?;
    let desc = if long_mode {
        let raw_high = bus.read_u64(table_base + byte_off + 8)?;
        parse_system_descriptor(raw_low, raw_high)
    } else {
        parse_descriptor_low(raw_low)
    };

    if desc.s {
        return Err(Exception::gp(selector));
    }
    if !desc.present {
        return Err(Exception::np(selector));
    }

    let access = (desc.typ as u32)
        | ((desc.s as u32) << 4)
        | ((desc.dpl as u32) << 5)
        | ((desc.present as u32) << 7)
        | ((desc.avl as u32) << 8)
        | ((desc.l as u32) << 9)
        | ((desc.db as u32) << 10)
        | ((desc.g as u32) << 11);

    let dst = match seg {
        SystemSeg::Ldtr => &mut state.tables.ldtr,
        SystemSeg::Tr => &mut state.tables.tr,
    };
    dst.selector = selector;
    dst.base = desc.base;
    dst.limit = desc.limit;
    dst.access = access;
    Ok(())
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
        load_segment_descriptor(state, bus, seg, selector, seg == Register::SS)?;
        state.set_rip(next_ip);
        return Ok(());
    }

    // MOV to/from control/debug registers.
    if instr.op_kind(0) == OpKind::Register && (is_ctrl_reg(instr.op0_register()) || is_debug_reg(instr.op0_register()))
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
    load_segment_descriptor(state, bus, seg, selector, seg == Register::SS)
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
    let off_size = (off_bits / 8) as u32;
    let off = pop_sized(state, bus, off_size)? & mask_bits(off_bits);
    let cs = pop_u16(state, bus)?;
    let sp = state.stack_ptr().wrapping_add(pop_imm as u64);
    state.set_stack_ptr(sp);
    far_jump(state, bus, cs, off)
}

fn push_sized<B: CpuBus>(state: &mut CpuState, bus: &mut B, val: u64, size: u32) -> Result<(), Exception> {
    let sp_bits = state.stack_ptr_bits();
    let mut sp = state.stack_ptr();
    sp = sp.wrapping_sub(size as u64) & mask_bits(sp_bits);
    state.set_stack_ptr(sp);
    let addr = state.seg_base_reg(Register::SS).wrapping_add(sp);
    write_mem(bus, addr, size * 8, val)
}

fn pop_sized<B: CpuBus>(state: &mut CpuState, bus: &mut B, size: u32) -> Result<u64, Exception> {
    let sp_bits = state.stack_ptr_bits();
    let sp = state.stack_ptr();
    let addr = state.seg_base_reg(Register::SS).wrapping_add(sp);
    let v = read_mem(bus, addr, size * 8)?;
    let next = sp.wrapping_add(size as u64) & mask_bits(sp_bits);
    state.set_stack_ptr(next);
    Ok(v)
}

fn far_jump<B: CpuBus>(state: &mut CpuState, bus: &mut B, selector: u16, offset: u64) -> Result<(), Exception> {
    match state.mode {
        CpuMode::Real | CpuMode::Vm86 => {
            set_real_mode_seg(&mut state.segments.cs, selector);
            state.set_rip(offset);
            Ok(())
        }
        CpuMode::Protected | CpuMode::Long => {
            load_segment_descriptor(state, bus, Register::CS, selector, false)?;
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
    let ret_size = (state.bitness() / 8) as u32;
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
    let limit = bus.read_u16(addr)?;
    let base = match state.bitness() {
        64 => bus.read_u64(addr + 2)?,
        16 => (bus.read_u32(addr + 2)? & 0x00FF_FFFF) as u64,
        _ => bus.read_u32(addr + 2)? as u64,
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
    bus.write_u16(addr, limit)?;
    match state.bitness() {
        64 => bus.write_u64(addr + 2, base)?,
        16 => bus.write_u32(addr + 2, base as u32 & 0x00FF_FFFF)?,
        _ => bus.write_u32(addr + 2, base as u32)?,
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
        Mnemonic::Ltr => load_system_segment(state, bus, SystemSeg::Tr, selector),
        Mnemonic::Lldt => load_system_segment(state, bus, SystemSeg::Ldtr, selector),
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
                    bus.write_u16(addr, msw)
                }
                _ => Err(Exception::InvalidOpcode),
            }
        }
        _ => Err(Exception::InvalidOpcode),
    }
}

fn instr_invlpg(ctx: &mut AssistContext, state: &CpuState, instr: &Instruction, next_ip: u64) -> Result<(), Exception> {
    require_cpl0(state)?;
    if instr.op_kind(0) != OpKind::Memory {
        return Err(Exception::InvalidOpcode);
    }
    let addr = calc_ea(state, instr, next_ip, true)?;
    ctx.invlpg_log.push(addr);
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

fn instr_syscall(state: &mut CpuState, return_ip: u64) -> Result<(), Exception> {
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
    if !is_canonical(target, state.mode.bitness() as u8) {
        return Err(Exception::gp0());
    }
    state.set_rip(target);
    Ok(())
}

fn instr_sysret(state: &mut CpuState) -> Result<(), Exception> {
    require_cpl0(state)?;
    if state.mode != CpuMode::Long {
        return Err(Exception::InvalidOpcode);
    }
    if (state.msr.efer & msr::EFER_SCE) == 0 {
        return Err(Exception::InvalidOpcode);
    }

    let target = state.read_reg(Register::RCX);
    if !is_canonical(target, state.mode.bitness() as u8) {
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
