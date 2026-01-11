//! Privileged/system instruction helpers.
//!
//! This is a purposely compact model of the parts of the x86 privileged ISA
//! that Windows 7 expects early during boot and during kernel runtime.

use crate::cpuid::{cpuid, CpuFeatures, CpuidResult};
use crate::exceptions::{Exception as ArchException, InterruptSource, PendingEvent};
use crate::fpu::FpKind;
use crate::msr::EFER_SCE;
use crate::time::TimeSource;
use crate::{msr::MsrState, CpuState, Exception, FxStateError, FXSAVE_AREA_SIZE};
use std::collections::VecDeque;

// --- Control register bit definitions (subset) ---
pub const CR0_MP: u64 = 1 << 1;
pub const CR0_EM: u64 = 1 << 2;
pub const CR0_TS: u64 = 1 << 3;
pub const CR0_NE: u64 = 1 << 5;

pub const CR4_OSFXSR: u64 = 1 << 9;
pub const CR4_OSXMMEXCPT: u64 = 1 << 10;

const MXCSR_EXCEPTION_MASK: u32 = 0x1F80;

/// A device/bus surface for port I/O instructions.
pub trait PortIo {
    fn port_read(&mut self, port: u16, size: u8) -> u32;
    fn port_write(&mut self, port: u16, size: u8, val: u32);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuMode {
    /// 16-bit real mode.
    Real,
    /// 32-bit protected mode (legacy).
    Protected32,
    /// 64-bit long mode.
    Long64,
}

/// GDTR/IDTR layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DescriptorTableRegister {
    pub limit: u16,
    pub base: u64,
}

/// Minimal subset of the legacy (32-bit) task state segment used for ring stack switching.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Tss32 {
    pub ss0: u16,
    pub esp0: u32,
    pub ss1: u16,
    pub esp1: u32,
    pub ss2: u16,
    pub esp2: u32,
}

impl Tss32 {
    pub fn stack_for_cpl(&self, cpl: u8) -> Option<(u16, u32)> {
        match cpl {
            0 => Some((self.ss0, self.esp0)),
            1 => Some((self.ss1, self.esp1)),
            2 => Some((self.ss2, self.esp2)),
            _ => None,
        }
    }
}

/// Minimal subset of the 64-bit task state segment used for stack switching (RSP{0,1,2} + IST).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Tss64 {
    pub rsp0: u64,
    pub rsp1: u64,
    pub rsp2: u64,
    pub ist: [u64; 7],
}

impl Tss64 {
    pub fn rsp_for_cpl(&self, cpl: u8) -> Option<u64> {
        match cpl {
            0 => Some(self.rsp0),
            1 => Some(self.rsp1),
            2 => Some(self.rsp2),
            _ => None,
        }
    }

    pub fn ist_stack(&self, ist: u8) -> Option<u64> {
        let idx = ist.checked_sub(1)? as usize;
        self.ist.get(idx).copied().filter(|val| *val != 0)
    }
}

/// Minimal CPU core state required by the system instruction surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cpu {
    pub mode: CpuMode,

    // General purpose registers.
    pub rax: u64,
    pub rbx: u64,
    pub rcx: u64,
    pub rdx: u64,
    pub rsi: u64,
    pub rdi: u64,
    pub rbp: u64,
    pub rsp: u64,
    pub r8: u64,
    pub r9: u64,
    pub r10: u64,
    pub r11: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,

    pub rip: u64,
    pub rflags: u64,

    // Segment selectors (hidden descriptor caches are out of scope here).
    pub cs: u16,
    pub ss: u16,
    pub ds: u16,
    pub es: u16,
    pub fs: u16,
    pub gs: u16,

    // Control registers.
    pub cr0: u64,
    pub cr2: u64,
    pub cr3: u64,
    pub cr4: u64,
    pub cr8: u64,

    // Debug registers.
    pub dr0: u64,
    pub dr1: u64,
    pub dr2: u64,
    pub dr3: u64,
    pub dr6: u64,
    pub dr7: u64,

    pub gdtr: DescriptorTableRegister,
    pub idtr: DescriptorTableRegister,
    pub ldtr: u16,
    pub tr: u16,

    pub msr: MsrState,
    pub features: CpuFeatures,
    pub time: TimeSource,

    pub halted: bool,
    /// Interrupt shadow after STI (inhibits interrupts for one instruction).
    pub interrupt_inhibit: u8,
    /// Used when `CR0.NE = 0` (x87 errors via external IRQ13) to allow the host
    /// to observe that a floating-point interrupt should be injected.
    pub irq13_pending: bool,

    /// Deferred exception/interrupt delivery (raised by interpreter/JIT slow paths).
    pub pending_event: Option<PendingEvent>,
    /// FIFO of externally injected interrupts (PIC/APIC).
    pub external_interrupts: VecDeque<u8>,

    /// Ring-stack configuration (normally sourced from the TSS).
    pub tss32: Option<Tss32>,
    pub tss64: Option<Tss64>,

    /// Tracks nested exception delivery for #DF escalation.
    pub(crate) exception_depth: u32,
    pub(crate) delivering_exception: Option<ArchException>,

    /// Tracks INVLPG calls for integration/testing.
    pub invlpg_log: Vec<u64>,
}

impl Cpu {
    pub const RFLAGS_FIXED1: u64 = 1 << 1;
    pub const RFLAGS_IF: u64 = 1 << 9;

    pub fn new(features: CpuFeatures) -> Self {
        Self {
            mode: CpuMode::Protected32,
            rax: 0,
            rbx: 0,
            rcx: 0,
            rdx: 0,
            rsi: 0,
            rdi: 0,
            rbp: 0,
            rsp: 0,
            r8: 0,
            r9: 0,
            r10: 0,
            r11: 0,
            r12: 0,
            r13: 0,
            r14: 0,
            r15: 0,
            rip: 0,
            rflags: Self::RFLAGS_FIXED1,
            cs: 0,
            ss: 0,
            ds: 0,
            es: 0,
            fs: 0,
            gs: 0,
            cr0: 0,
            cr2: 0,
            cr3: 0,
            cr4: 0,
            cr8: 0,
            dr0: 0,
            dr1: 0,
            dr2: 0,
            dr3: 0,
            dr6: 0,
            dr7: 0,
            gdtr: DescriptorTableRegister::default(),
            idtr: DescriptorTableRegister::default(),
            ldtr: 0,
            tr: 0,
            msr: MsrState::default(),
            features,
            time: TimeSource::default(),
            halted: false,
            interrupt_inhibit: 0,
            irq13_pending: false,
            pending_event: None,
            external_interrupts: VecDeque::new(),
            tss32: None,
            tss64: None,
            exception_depth: 0,
            delivering_exception: None,
            invlpg_log: Vec::new(),
        }
    }

    /// Inhibit maskable interrupts for exactly one instruction.
    ///
    /// This is used by `STI` (interrupt shadow) as well as `MOV SS`/`POP SS`
    /// semantics. The execution engine should call [`Cpu::retire_instruction`]
    /// after each successfully executed instruction to age this counter.
    pub fn inhibit_interrupts_for_one_instruction(&mut self) {
        self.interrupt_inhibit = 1;
    }

    /// Queue a software interrupt (`INT n`/`INT3`/`INT1`/`INTO`) for delivery at
    /// the next instruction boundary.
    ///
    /// This mirrors the Tier-0 interrupt delivery model where instruction
    /// decoding raises a pending event and the dispatcher delivers it before the
    /// next instruction executes.
    pub fn raise_software_interrupt(&mut self, vector: u8, return_rip: u64) {
        self.pending_event = Some(PendingEvent::Interrupt {
            vector,
            saved_rip: return_rip,
            source: InterruptSource::Software,
        });
    }

    /// Deliver any queued exception/interrupt event.
    ///
    /// The [`Cpu`] model is used primarily by unit-test harnesses that run
    /// real-mode code, so this currently implements only real-mode IVT delivery.
    pub fn deliver_pending_event<B: crate::CpuBus>(&mut self, bus: &mut B) -> Result<(), Exception> {
        let Some(event) = self.pending_event.take() else {
            return Ok(());
        };

        match event {
            PendingEvent::Interrupt {
                vector,
                saved_rip,
                source: _,
            } => {
                if self.mode != CpuMode::Real {
                    return Err(Exception::Unimplemented(
                        "system::Cpu interrupt delivery outside real mode",
                    ));
                }

                fn push_u16<B: crate::CpuBus>(
                    cpu: &mut Cpu,
                    bus: &mut B,
                    val: u16,
                ) -> Result<(), Exception> {
                    let sp = (cpu.rsp as u16).wrapping_sub(2);
                    cpu.rsp = (cpu.rsp & !0xFFFF) | sp as u64;
                    let addr = ((cpu.ss as u64) << 4).wrapping_add(sp as u64);
                    bus.write_u16(addr, val)?;
                    Ok(())
                }

                let flags = self.rflags as u16;
                let cs = self.cs;
                let ip = (saved_rip & 0xFFFF) as u16;

                push_u16(self, bus, flags)?;
                push_u16(self, bus, cs)?;
                push_u16(self, bus, ip)?;

                // Clear IF + TF (interrupt gate behavior).
                self.set_rflags(self.rflags & !(Self::RFLAGS_IF | (1 << 8)));

                let ivt = (vector as u64) * 4;
                let new_ip = bus.read_u16(ivt)? as u64;
                let new_cs = bus.read_u16(ivt + 2)?;

                self.cs = new_cs;
                self.rip = new_ip;
                Ok(())
            }
            _ => Err(Exception::Unimplemented("system::Cpu pending event delivery")),
        }
    }

    /// Execute an `IRET` return from an interrupt handler.
    pub fn iret<B: crate::CpuBus>(&mut self, bus: &mut B) -> Result<(), Exception> {
        if self.mode != CpuMode::Real {
            return Err(Exception::Unimplemented("system::Cpu IRET outside real mode"));
        }

        fn pop_u16<B: crate::CpuBus>(cpu: &mut Cpu, bus: &mut B) -> Result<u16, Exception> {
            let sp = cpu.rsp as u16;
            let addr = ((cpu.ss as u64) << 4).wrapping_add(sp as u64);
            let val = bus.read_u16(addr)?;
            let new_sp = sp.wrapping_add(2);
            cpu.rsp = (cpu.rsp & !0xFFFF) | new_sp as u64;
            Ok(val)
        }

        let ip = pop_u16(self, bus)?;
        let cs = pop_u16(self, bus)?;
        let flags = pop_u16(self, bus)?;

        self.cs = cs;
        self.rip = ip as u64;

        // IRET restores FLAGS (16-bit in real mode), preserving upper bits.
        let new_flags = (self.rflags & !0xFFFF) | (flags as u64);
        self.set_rflags(new_flags);
        Ok(())
    }

    /// Current privilege level (CPL), derived from CS.RPL.
    #[inline]
    pub fn cpl(&self) -> u8 {
        // Real mode has no privilege rings; treat all code as CPL0.
        if self.mode == CpuMode::Real {
            0
        } else {
            (self.cs & 0b11) as u8
        }
    }

    #[inline]
    pub fn iopl(&self) -> u8 {
        ((self.rflags >> 12) & 0b11) as u8
    }

    /// Raise `#GP(0)` unless CPL==0.
    #[inline]
    pub fn require_cpl0(&self) -> Result<(), Exception> {
        if self.cpl() != 0 {
            Err(Exception::gp0())
        } else {
            Ok(())
        }
    }

    /// Raise `#GP(0)` unless CPL<=IOPL.
    #[inline]
    pub fn require_iopl(&self) -> Result<(), Exception> {
        // In real mode, IOPL checks are not enforced.
        if self.mode == CpuMode::Real {
            return Ok(());
        }
        if self.cpl() > self.iopl() {
            Err(Exception::gp0())
        } else {
            Ok(())
        }
    }

    #[inline]
    fn set_rflags(&mut self, value: u64) {
        // RFLAGS bit 1 is always 1.
        self.rflags = value | Self::RFLAGS_FIXED1;
    }

    #[inline]
    fn is_canonical(addr: u64) -> bool {
        // Canonical if bits 63:48 are sign-extension of bit 47.
        let sign = (addr >> 47) & 1;
        let upper = addr >> 48;
        if sign == 0 {
            upper == 0
        } else {
            upper == 0xFFFF
        }
    }

    // --- Control/debug register moves ---

    pub fn mov_from_cr(&self, cr: u8) -> Result<u64, Exception> {
        self.require_cpl0()?;
        match cr {
            0 => Ok(self.cr0),
            2 => Ok(self.cr2),
            3 => Ok(self.cr3),
            4 => Ok(self.cr4),
            8 => Ok(self.cr8),
            _ => Err(Exception::gp0()),
        }
    }

    pub fn mov_to_cr(&mut self, cr: u8, val: u64) -> Result<(), Exception> {
        self.require_cpl0()?;
        match cr {
            0 => self.cr0 = val,
            2 => self.cr2 = val,
            3 => self.cr3 = val,
            4 => self.cr4 = val,
            8 => self.cr8 = val,
            _ => return Err(Exception::gp0()),
        }
        Ok(())
    }

    pub fn mov_from_dr(&self, dr: u8) -> Result<u64, Exception> {
        self.require_cpl0()?;
        match dr {
            0 => Ok(self.dr0),
            1 => Ok(self.dr1),
            2 => Ok(self.dr2),
            3 => Ok(self.dr3),
            6 => Ok(self.dr6),
            7 => Ok(self.dr7),
            _ => Err(Exception::gp0()),
        }
    }

    pub fn mov_to_dr(&mut self, dr: u8, val: u64) -> Result<(), Exception> {
        self.require_cpl0()?;
        match dr {
            0 => self.dr0 = val,
            1 => self.dr1 = val,
            2 => self.dr2 = val,
            3 => self.dr3 = val,
            6 => self.dr6 = val,
            7 => self.dr7 = val,
            _ => return Err(Exception::gp0()),
        }
        Ok(())
    }

    pub fn clts(&mut self) -> Result<(), Exception> {
        self.require_cpl0()?;
        // CR0.TS is bit 3.
        self.cr0 &= !CR0_TS;
        Ok(())
    }

    /// Architectural gating for x87/MMX/SSE operations.
    ///
    /// This models the subset of CR0/CR4 semantics that Windows relies on for
    /// lazy FP context switching:
    /// * `CR0.EM = 1` disables the x87 (and, by policy, MMX/SSE) and raises `#UD`.
    /// * `CR4.OSFXSR = 0` disables SSE/FXSAVE/FXRSTOR/MXCSR access and raises `#UD`.
    /// * `CR0.TS = 1` causes x87/MMX/SSE to raise `#NM`.
    pub fn check_fp_available(&mut self, kind: FpKind) -> Result<(), Exception> {
        // Give #UD priority over #NM: if the ISA is disabled entirely, we do not
        // report it as a lazy-FPU trap.
        if (self.cr0 & CR0_EM) != 0 {
            return Err(Exception::InvalidOpcode);
        }

        if matches!(kind, FpKind::Sse) && (self.cr4 & CR4_OSFXSR) == 0 {
            return Err(Exception::InvalidOpcode);
        }

        if (self.cr0 & CR0_TS) != 0 {
            return Err(Exception::DeviceNotAvailable);
        }

        Ok(())
    }

    /// WAIT/FWAIT (0x9B).
    ///
    /// SDM: If `CR0.MP = 1` and `CR0.TS = 1`, `WAIT/FWAIT` raises `#NM`.
    pub fn instr_wait(&mut self, fp: &crate::fpu::FpuState) -> Result<(), Exception> {
        if (self.cr0 & CR0_EM) != 0 {
            return Err(Exception::InvalidOpcode);
        }

        if (self.cr0 & CR0_MP) != 0 && (self.cr0 & CR0_TS) != 0 {
            return Err(Exception::DeviceNotAvailable);
        }

        // Simplified x87 exception delivery.
        if fp.has_unmasked_exception() {
            if (self.cr0 & CR0_NE) != 0 {
                return Err(Exception::X87Fpu);
            }

            self.irq13_pending = true;
        }

        Ok(())
    }

    /// Representative x87 instruction wrapper (`FNINIT`/`FINIT`).
    pub fn instr_fninit(&mut self, state: &mut CpuState) -> Result<(), Exception> {
        self.check_fp_available(FpKind::X87)?;
        state.fninit();
        Ok(())
    }

    /// Representative SSE instruction wrapper (`XORPS xmm, xmm`).
    pub fn instr_xorps(
        &mut self,
        state: &mut CpuState,
        dst: usize,
        src: usize,
    ) -> Result<(), Exception> {
        self.check_fp_available(FpKind::Sse)?;
        let src_val = *state.sse.xmm.get(src).ok_or(Exception::InvalidOpcode)?;
        let dst_reg = state
            .sse
            .xmm
            .get_mut(dst)
            .ok_or(Exception::InvalidOpcode)?;
        *dst_reg ^= src_val;
        Ok(())
    }

    /// Instruction-level `STMXCSR` wrapper with CR0/CR4 gating.
    pub fn instr_stmxcsr(&mut self, state: &CpuState, dst: &mut [u8; 4]) -> Result<(), Exception> {
        self.check_fp_available(FpKind::Sse)?;
        state.stmxcsr(dst);
        Ok(())
    }

    /// Instruction-level `LDMXCSR` wrapper with CR0/CR4 gating.
    pub fn instr_ldmxcsr(&mut self, state: &mut CpuState, src: &[u8; 4]) -> Result<(), Exception> {
        self.check_fp_available(FpKind::Sse)?;
        let mut value = u32::from_le_bytes(*src);
        if (self.cr4 & CR4_OSXMMEXCPT) == 0 {
            // If the OS hasn't opted into SIMD FP exception delivery, ensure all
            // exception masks remain set so we never need to inject #XM/#XF.
            value |= MXCSR_EXCEPTION_MASK;
        }
        let bytes = value.to_le_bytes();
        state.ldmxcsr(&bytes).map_err(map_fx_state_error)
    }

    /// Instruction-level `FXSAVE` wrapper with CR0/CR4 gating.
    pub fn instr_fxsave(
        &mut self,
        state: &CpuState,
        dst: &mut [u8; FXSAVE_AREA_SIZE],
    ) -> Result<(), Exception> {
        self.check_fp_available(FpKind::Sse)?;
        state.fxsave(dst);
        Ok(())
    }

    /// Instruction-level `FXRSTOR` wrapper with CR0/CR4 gating.
    pub fn instr_fxrstor(
        &mut self,
        state: &mut CpuState,
        src: &[u8; FXSAVE_AREA_SIZE],
    ) -> Result<(), Exception> {
        self.check_fp_available(FpKind::Sse)?;

        if (self.cr4 & CR4_OSXMMEXCPT) == 0 {
            let mut patched = *src;
            let mut mxcsr_bytes = [0u8; 4];
            mxcsr_bytes.copy_from_slice(&patched[24..28]);
            let mxcsr = u32::from_le_bytes(mxcsr_bytes) | MXCSR_EXCEPTION_MASK;
            patched[24..28].copy_from_slice(&mxcsr.to_le_bytes());
            state.fxrstor(&patched).map_err(map_fx_state_error)
        } else {
            state.fxrstor(src).map_err(map_fx_state_error)
        }
    }

    /// Store the machine status word (lower 16 bits of CR0).
    pub fn smsw(&self) -> u16 {
        (self.cr0 & 0xFFFF) as u16
    }

    pub fn lmsw(&mut self, val: u16) -> Result<(), Exception> {
        self.require_cpl0()?;
        // LMSW updates CR0 bits 0-3 from val (PE, MP, EM, TS).
        let mask: u64 = 0b1111;
        self.cr0 = (self.cr0 & !mask) | (val as u64 & mask);
        Ok(())
    }

    pub fn invlpg(&mut self, addr: u64) -> Result<(), Exception> {
        self.require_cpl0()?;
        self.invlpg_log.push(addr);
        Ok(())
    }

    // --- Descriptor tables / task state ---

    pub fn lgdt(&mut self, gdtr: DescriptorTableRegister) -> Result<(), Exception> {
        self.require_cpl0()?;
        self.gdtr = gdtr;
        Ok(())
    }

    pub fn sgdt(&self) -> DescriptorTableRegister {
        self.gdtr
    }

    pub fn lidt(&mut self, idtr: DescriptorTableRegister) -> Result<(), Exception> {
        self.require_cpl0()?;
        self.idtr = idtr;
        Ok(())
    }

    pub fn sidt(&self) -> DescriptorTableRegister {
        self.idtr
    }

    pub fn lldt(&mut self, selector: u16) -> Result<(), Exception> {
        self.require_cpl0()?;
        self.ldtr = selector;
        Ok(())
    }

    pub fn sldt(&self) -> u16 {
        self.ldtr
    }

    pub fn ltr(&mut self, selector: u16) -> Result<(), Exception> {
        self.require_cpl0()?;
        self.tr = selector;
        Ok(())
    }

    pub fn str(&self) -> u16 {
        self.tr
    }

    // --- Interrupt flag and halt ---

    pub fn cli(&mut self) -> Result<(), Exception> {
        self.require_iopl()?;
        self.set_rflags(self.rflags & !Self::RFLAGS_IF);
        Ok(())
    }

    pub fn sti(&mut self) -> Result<(), Exception> {
        self.require_iopl()?;
        self.set_rflags(self.rflags | Self::RFLAGS_IF);
        // Interrupt shadow for one instruction.
        self.inhibit_interrupts_for_one_instruction();
        Ok(())
    }

    pub fn hlt(&mut self) -> Result<(), Exception> {
        self.require_cpl0()?;
        self.halted = true;
        Ok(())
    }

    /// Call after each successfully executed instruction to update the interrupt shadow and
    /// advance the virtual time source.
    pub fn retire_instruction(&mut self) {
        self.retire_cycles(1);
    }

    /// Retire an instruction that took `cycles` virtual cycles.
    pub fn retire_cycles(&mut self, cycles: u64) {
        if self.interrupt_inhibit > 0 {
            self.interrupt_inhibit -= 1;
        }
        self.time.advance_cycles(cycles);
    }

    // --- CPUID ---

    pub fn cpuid_query(&self, leaf: u32, subleaf: u32) -> CpuidResult {
        cpuid(&self.features, leaf, subleaf)
    }

    /// CPUID instruction semantics (reads EAX/ECX, writes EAX/EBX/ECX/EDX).
    pub fn instr_cpuid(&mut self) {
        let leaf = self.rax as u32;
        let subleaf = self.rcx as u32;
        let res = self.cpuid_query(leaf, subleaf);
        self.rax = res.eax as u64;
        self.rbx = res.ebx as u64;
        self.rcx = res.ecx as u64;
        self.rdx = res.edx as u64;
    }

    // --- Time / serialization primitives ---

    /// `RDTSC` instruction semantics (writes EDX:EAX).
    pub fn instr_rdtsc(&mut self) {
        let tsc = self.time.read_tsc();
        self.rax = (tsc as u32) as u64;
        self.rdx = ((tsc >> 32) as u32) as u64;
    }

    /// `RDTSCP` instruction semantics (writes EDX:EAX and ECX = IA32_TSC_AUX).
    pub fn instr_rdtscp(&mut self) {
        let tsc = self.time.read_tsc();
        self.rax = (tsc as u32) as u64;
        self.rdx = ((tsc >> 32) as u32) as u64;
        self.rcx = self.msr.tsc_aux as u64;
    }

    /// `LFENCE` acts as a serializing point for the virtual pipeline.
    #[inline]
    pub fn instr_lfence(&mut self) {}

    /// `SFENCE` acts as a serializing point for the virtual pipeline.
    #[inline]
    pub fn instr_sfence(&mut self) {}

    /// `MFENCE` acts as a serializing point for the virtual pipeline.
    #[inline]
    pub fn instr_mfence(&mut self) {}

    /// `PAUSE` is a no-op in the interpreter with a spin-loop hint.
    #[inline]
    pub fn instr_pause(&mut self) {
        core::hint::spin_loop();
    }

    /// Decode and execute a time/serialization primitive instruction.
    ///
    /// This is intentionally a narrow helper for instruction encodings that are relied upon by
    /// Windows 7 early during boot/runtime (RDTSC/RDTSCP, fences, PAUSE, CPUID, RDMSR/WRMSR).
    ///
    /// The caller is responsible for advancing `rip` and calling `retire_cycles(inst.cycles)`
    /// after successful execution.
    pub fn exec_time_insn(
        &mut self,
        bytes: &[u8],
    ) -> Result<crate::time_insn::DecodedInstruction, Exception> {
        use crate::time_insn::InstructionKind;

        let inst = crate::time_insn::decode_instruction(bytes)?;
        match inst.kind {
            InstructionKind::Rdtsc => self.instr_rdtsc(),
            InstructionKind::Rdtscp => self.instr_rdtscp(),
            InstructionKind::Lfence => self.instr_lfence(),
            InstructionKind::Sfence => self.instr_sfence(),
            InstructionKind::Mfence => self.instr_mfence(),
            InstructionKind::Cpuid => self.instr_cpuid(),
            InstructionKind::Pause => self.instr_pause(),
            InstructionKind::Nop => {}
            InstructionKind::Rdmsr => {
                self.instr_rdmsr()?;
            }
            InstructionKind::Wrmsr => {
                self.instr_wrmsr()?;
            }
        }

        Ok(inst)
    }

    // --- MSR ---

    pub fn rdmsr_value(&mut self, msr_index: u32) -> Result<u64, Exception> {
        self.require_cpl0()?;
        if msr_index == crate::msr::IA32_TSC {
            return Ok(self.time.read_tsc());
        }
        self.msr.read(msr_index)
    }

    pub fn wrmsr_value(&mut self, msr_index: u32, value: u64) -> Result<(), Exception> {
        self.require_cpl0()?;
        if msr_index == crate::msr::IA32_TSC {
            self.time.set_tsc(value);
            return Ok(());
        }
        self.msr.write(&self.features, msr_index, value)
    }

    /// RDMSR instruction semantics (ECX selects MSR, value returned in EDX:EAX).
    pub fn instr_rdmsr(&mut self) -> Result<(), Exception> {
        let msr_index = self.rcx as u32;
        let value = self.rdmsr_value(msr_index)?;
        self.rax = (value as u32) as u64;
        self.rdx = ((value >> 32) as u32) as u64;
        Ok(())
    }

    /// WRMSR instruction semantics (ECX selects MSR, value from EDX:EAX).
    pub fn instr_wrmsr(&mut self) -> Result<(), Exception> {
        let msr_index = self.rcx as u32;
        let value = ((self.rdx as u64) << 32) | (self.rax as u32 as u64);
        self.wrmsr_value(msr_index, value)
    }

    // --- Fast syscalls ---

    pub fn syscall(&mut self) -> Result<(), Exception> {
        if self.mode != CpuMode::Long64 {
            return Err(Exception::InvalidOpcode);
        }
        if (self.msr.efer & EFER_SCE) == 0 {
            return Err(Exception::InvalidOpcode);
        }

        let return_rip = self.rip.wrapping_add(2);
        self.rcx = return_rip;
        self.r11 = self.rflags;

        let star = self.msr.star;
        let syscall_cs = ((star >> 32) & 0xFFFF) as u16;
        self.cs = syscall_cs & !0b11;
        self.ss = syscall_cs.wrapping_add(8) & !0b11;

        let fmask = self.msr.fmask;
        self.set_rflags(self.rflags & !fmask);

        let target = self.msr.lstar;
        if !Self::is_canonical(target) {
            return Err(Exception::gp0());
        }
        self.rip = target;
        Ok(())
    }

    pub fn sysret(&mut self) -> Result<(), Exception> {
        self.require_cpl0()?;
        if self.mode != CpuMode::Long64 {
            return Err(Exception::InvalidOpcode);
        }
        if (self.msr.efer & EFER_SCE) == 0 {
            return Err(Exception::InvalidOpcode);
        }

        let target = self.rcx;
        if !Self::is_canonical(target) {
            return Err(Exception::gp0());
        }

        // Restore flags first.
        self.set_rflags(self.r11);

        let star = self.msr.star;
        // Per Intel/AMD SDM: SYSRET loads CS = STAR[63:48] + 16, SS = STAR[63:48] + 8.
        // RPL is forced to 3.
        let sysret_base = ((star >> 48) & 0xFFFF) as u16;
        let user_cs = sysret_base.wrapping_add(16);
        let user_ss = sysret_base.wrapping_add(8);
        self.cs = (user_cs & !0b11) | 0b11;
        self.ss = (user_ss & !0b11) | 0b11;

        self.rip = target;
        Ok(())
    }

    pub fn sysenter(&mut self) -> Result<(), Exception> {
        // SYSENTER is intended as a fast transition from user to kernel.
        // It is not privileged, but the MSRs must be configured.
        if self.mode == CpuMode::Real {
            return Err(Exception::InvalidOpcode);
        }

        let cs = self.msr.sysenter_cs as u16;
        if cs == 0 {
            return Err(Exception::gp0());
        }
        self.cs = cs & !0b11;
        self.ss = self.cs.wrapping_add(8) & !0b11;
        self.rsp = self.msr.sysenter_esp;
        self.rip = self.msr.sysenter_eip;
        Ok(())
    }

    pub fn sysexit(&mut self) -> Result<(), Exception> {
        self.require_cpl0()?;
        if self.mode == CpuMode::Real {
            return Err(Exception::InvalidOpcode);
        }

        let cs = self.msr.sysenter_cs as u16;
        if cs == 0 {
            return Err(Exception::gp0());
        }

        let cs_base = cs & !0b11;
        self.cs = cs_base.wrapping_add(16) | 0b11;
        self.ss = cs_base.wrapping_add(24) | 0b11;

        // Intel SDM: in 32-bit mode, `SYSEXIT` loads EIP from EDX and ESP from ECX.
        // In long mode it uses RCX/RDX.
        match self.mode {
            CpuMode::Protected32 => {
                self.rip = (self.rdx as u32) as u64;
                self.rsp = (self.rcx as u32) as u64;
            }
            CpuMode::Long64 => {
                self.rip = self.rcx;
                self.rsp = self.rdx;
            }
            CpuMode::Real => unreachable!("handled above"),
        }
        Ok(())
    }

    pub fn swapgs(&mut self) -> Result<(), Exception> {
        self.require_cpl0()?;
        if self.mode != CpuMode::Long64 {
            return Err(Exception::InvalidOpcode);
        }
        core::mem::swap(&mut self.msr.gs_base, &mut self.msr.kernel_gs_base);
        Ok(())
    }

    // --- Port I/O ---

    pub fn io_in(&mut self, port: u16, size: u8, io: &mut impl PortIo) -> Result<u32, Exception> {
        self.require_iopl()?;
        Ok(io.port_read(port, size))
    }

    pub fn io_out(
        &mut self,
        port: u16,
        size: u8,
        val: u32,
        io: &mut impl PortIo,
    ) -> Result<(), Exception> {
        self.require_iopl()?;
        io.port_write(port, size, val);
        Ok(())
    }

    /// IN instruction semantics writing into AL/AX/EAX.
    pub fn instr_in(&mut self, port: u16, size: u8, io: &mut impl PortIo) -> Result<(), Exception> {
        let val = self.io_in(port, size, io)?;
        match size {
            1 => {
                self.rax = (self.rax & !0xFF) | (val as u64 & 0xFF);
            }
            2 => {
                self.rax = (self.rax & !0xFFFF) | (val as u64 & 0xFFFF);
            }
            4 => {
                self.rax = (val as u32) as u64;
            }
            _ => return Err(Exception::InvalidOpcode),
        }
        Ok(())
    }

    /// OUT instruction semantics reading from AL/AX/EAX.
    pub fn instr_out(
        &mut self,
        port: u16,
        size: u8,
        io: &mut impl PortIo,
    ) -> Result<(), Exception> {
        let val = match size {
            1 => (self.rax & 0xFF) as u32,
            2 => (self.rax & 0xFFFF) as u32,
            4 => self.rax as u32,
            _ => return Err(Exception::InvalidOpcode),
        };
        self.io_out(port, size, val, io)
    }

    // --- String I/O stubs ---

    pub fn instr_ins(&mut self) -> Result<(), Exception> {
        Err(Exception::Unimplemented("INS/OUTS"))
    }

    pub fn instr_outs(&mut self) -> Result<(), Exception> {
        Err(Exception::Unimplemented("INS/OUTS"))
    }
}

impl Default for Cpu {
    fn default() -> Self {
        Self::new(CpuFeatures::default())
    }
}

fn map_fx_state_error(err: FxStateError) -> Exception {
    match err {
        // SDM: Loading an MXCSR value with reserved bits set raises #GP(0).
        FxStateError::MxcsrReservedBits { .. } => Exception::gp0(),
    }
}
