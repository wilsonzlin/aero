//! Privileged/system instruction helpers.
//!
//! This is a purposely compact model of the parts of the x86 privileged ISA
//! that Windows 7 expects early during boot and during kernel runtime.

use crate::cpuid::{cpuid, CpuFeatures, CpuidResult};
use crate::msr::EFER_SCE;
use crate::{msr::MsrState, Exception};

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

    pub halted: bool,
    /// Interrupt shadow after STI (inhibits interrupts for one instruction).
    pub interrupt_inhibit: u8,

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
            halted: false,
            interrupt_inhibit: 0,
            invlpg_log: Vec::new(),
        }
    }

    /// Current privilege level (CPL), derived from CS.RPL.
    #[inline]
    pub fn cpl(&self) -> u8 {
        (self.cs & 0b11) as u8
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
        self.cr0 &= !(1 << 3);
        Ok(())
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
        self.interrupt_inhibit = 1;
        Ok(())
    }

    pub fn hlt(&mut self) -> Result<(), Exception> {
        self.require_cpl0()?;
        self.halted = true;
        Ok(())
    }

    /// Call after each successfully executed instruction to update the interrupt shadow.
    pub fn retire_instruction(&mut self) {
        if self.interrupt_inhibit > 0 {
            self.interrupt_inhibit -= 1;
        }
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

    // --- MSR ---

    pub fn rdmsr_value(&mut self, msr_index: u32) -> Result<u64, Exception> {
        self.require_cpl0()?;
        self.msr.read(msr_index)
    }

    pub fn wrmsr_value(&mut self, msr_index: u32, value: u64) -> Result<(), Exception> {
        self.require_cpl0()?;
        self.msr.write(msr_index, value)
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
        let sysret_cs = ((star >> 48) & 0xFFFF) as u16;
        self.cs = (sysret_cs & !0b11) | 0b11;
        self.ss = (sysret_cs.wrapping_add(8) & !0b11) | 0b11;

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

        // Return target and stack are provided in ECX/EDX.
        self.rip = (self.rcx as u32) as u64;
        self.rsp = (self.rdx as u32) as u64;
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
