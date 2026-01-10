#![forbid(unsafe_code)]

use aero_core::memory::Memory;

use std::collections::VecDeque;

use crate::descriptors::{
    parse_segment_descriptor, parse_system_descriptor, parse_system_descriptor_64,
    DescriptorTableReg,
};
use crate::msr::{MsrError, IA32_KERNEL_GS_BASE};
use crate::msr::{Msrs, EFER_LMA, EFER_LME, EFER_SCE, IA32_EFER, IA32_FS_BASE, IA32_GS_BASE};

pub const CR0_PE: u64 = 1 << 0;
pub const CR0_PG: u64 = 1 << 31;

pub const CR4_PAE: u64 = 1 << 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegReg {
    Es,
    Cs,
    Ss,
    Ds,
    Fs,
    Gs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gpr {
    Rax = 0,
    Rcx = 1,
    Rdx = 2,
    Rbx = 3,
    Rsp = 4,
    Rbp = 5,
    Rsi = 6,
    Rdi = 7,
    R8 = 8,
    R9 = 9,
    R10 = 10,
    R11 = 11,
    R12 = 12,
    R13 = 13,
    R14 = 14,
    R15 = 15,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SegmentCache {
    pub base: u64,
    pub limit: u32,
    pub access: u8,
    pub flags: u8,
}

impl SegmentCache {
    pub fn unusable() -> Self {
        Self {
            base: 0,
            limit: 0,
            access: 0,
            flags: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SegmentRegister {
    pub selector: u16,
    pub cache: SegmentCache,
}

impl SegmentRegister {
    pub fn null() -> Self {
        Self {
            selector: 0,
            cache: SegmentCache::unusable(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SystemSegmentRegister {
    pub selector: u16,
    pub base: u64,
    pub limit: u32,
    pub access: u8,
    pub flags: u8,
}

impl SystemSegmentRegister {
    pub fn null() -> Self {
        Self {
            selector: 0,
            base: 0,
            limit: 0,
            access: 0,
            flags: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControlRegs {
    pub cr0: u64,
    pub cr2: u64,
    pub cr3: u64,
    pub cr4: u64,
    pub cr8: u64,
}

impl Default for ControlRegs {
    fn default() -> Self {
        Self {
            cr0: 0,
            cr2: 0,
            cr3: 0,
            cr4: 0,
            cr8: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DebugRegs {
    pub dr: [u64; 8],
}

impl Default for DebugRegs {
    fn default() -> Self {
        Self { dr: [0; 8] }
    }
}

pub const RFLAGS_CF: u64 = 1 << 0;
pub const RFLAGS_PF: u64 = 1 << 2;
pub const RFLAGS_AF: u64 = 1 << 4;
pub const RFLAGS_ZF: u64 = 1 << 6;
pub const RFLAGS_SF: u64 = 1 << 7;
pub const RFLAGS_TF: u64 = 1 << 8;
pub const RFLAGS_IF: u64 = 1 << 9;
pub const RFLAGS_DF: u64 = 1 << 10;
pub const RFLAGS_OF: u64 = 1 << 11;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LazyFlags {
    Add {
        lhs: u64,
        rhs: u64,
        width: u8,
        result: u64,
    },
    Sub {
        lhs: u64,
        rhs: u64,
        width: u8,
        result: u64,
    },
    Logic {
        result: u64,
        width: u8,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rflags {
    raw: u64,
    lazy: Option<LazyFlags>,
}

impl Default for Rflags {
    fn default() -> Self {
        Self {
            raw: 1 << 1,
            lazy: None,
        }
    }
}

impl Rflags {
    pub fn set_raw(&mut self, val: u64) {
        self.raw = val | (1 << 1);
        self.lazy = None;
    }

    pub fn set_lazy(&mut self, lazy: LazyFlags) {
        self.lazy = Some(lazy);
    }

    pub fn read(&mut self) -> u64 {
        if let Some(lazy) = self.lazy.take() {
            let old = self.raw;
            let computed = match lazy {
                LazyFlags::Add {
                    lhs,
                    rhs,
                    width,
                    result,
                } => compute_add_flags(old, lhs, rhs, width, result),
                LazyFlags::Sub {
                    lhs,
                    rhs,
                    width,
                    result,
                } => compute_sub_flags(old, lhs, rhs, width, result),
                LazyFlags::Logic { result, width } => compute_logic_flags(old, width, result),
            };
            self.raw = computed | (1 << 1);
        }
        self.raw | (1 << 1)
    }

    pub fn write(&mut self, val: u64) {
        self.set_raw(val);
    }

    pub fn if_flag(&mut self) -> bool {
        self.read() & RFLAGS_IF != 0
    }

    pub fn set_if(&mut self, val: bool) {
        let mut raw = self.read();
        if val {
            raw |= RFLAGS_IF;
        } else {
            raw &= !RFLAGS_IF;
        }
        self.raw = raw;
    }
}

fn mask_width(width: u8) -> u64 {
    match width {
        8 => 0xFF,
        16 => 0xFFFF,
        32 => 0xFFFF_FFFF,
        64 => 0xFFFF_FFFF_FFFF_FFFF,
        other => panic!("unsupported width {other}"),
    }
}

fn parity8(x: u8) -> bool {
    x.count_ones() % 2 == 0
}

fn compute_logic_flags(old: u64, width: u8, result: u64) -> u64 {
    let mask = mask_width(width);
    let res = result & mask;
    let mut flags = old & !(RFLAGS_CF | RFLAGS_PF | RFLAGS_AF | RFLAGS_ZF | RFLAGS_SF | RFLAGS_OF);

    if res == 0 {
        flags |= RFLAGS_ZF;
    }
    if res & (1u64 << (width - 1)) != 0 {
        flags |= RFLAGS_SF;
    }
    if parity8(res as u8) {
        flags |= RFLAGS_PF;
    }
    // CF/OF cleared, AF undefined (we clear).
    flags
}

fn compute_add_flags(old: u64, lhs: u64, rhs: u64, width: u8, result: u64) -> u64 {
    let mask = mask_width(width);
    let res = result & mask;
    let lhs = lhs & mask;
    let rhs = rhs & mask;
    let mut flags = old & !(RFLAGS_CF | RFLAGS_PF | RFLAGS_AF | RFLAGS_ZF | RFLAGS_SF | RFLAGS_OF);

    if res == 0 {
        flags |= RFLAGS_ZF;
    }
    if res & (1u64 << (width - 1)) != 0 {
        flags |= RFLAGS_SF;
    }
    if parity8(res as u8) {
        flags |= RFLAGS_PF;
    }

    if (lhs as u128 + rhs as u128) > mask as u128 {
        flags |= RFLAGS_CF;
    }

    // AF: carry out of bit 3
    if ((lhs ^ rhs ^ res) & 0x10) != 0 {
        flags |= RFLAGS_AF;
    }

    // OF for addition: (~(lhs ^ rhs) & (lhs ^ res)) sign bit set
    let sign = 1u64 << (width - 1);
    if (((!(lhs ^ rhs)) & (lhs ^ res)) & sign) != 0 {
        flags |= RFLAGS_OF;
    }

    flags
}

fn compute_sub_flags(old: u64, lhs: u64, rhs: u64, width: u8, result: u64) -> u64 {
    let mask = mask_width(width);
    let res = result & mask;
    let lhs = lhs & mask;
    let rhs = rhs & mask;
    let mut flags = old & !(RFLAGS_CF | RFLAGS_PF | RFLAGS_AF | RFLAGS_ZF | RFLAGS_SF | RFLAGS_OF);

    if res == 0 {
        flags |= RFLAGS_ZF;
    }
    if res & (1u64 << (width - 1)) != 0 {
        flags |= RFLAGS_SF;
    }
    if parity8(res as u8) {
        flags |= RFLAGS_PF;
    }

    if lhs < rhs {
        flags |= RFLAGS_CF;
    }

    if ((lhs ^ rhs ^ res) & 0x10) != 0 {
        flags |= RFLAGS_AF;
    }

    // OF for subtraction: ((lhs ^ rhs) & (lhs ^ res)) sign bit set
    let sign = 1u64 << (width - 1);
    if (((lhs ^ rhs) & (lhs ^ res)) & sign) != 0 {
        flags |= RFLAGS_OF;
    }

    flags
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Exception {
    DivideError,
    InvalidOpcode,
    GeneralProtection { code: u32 },
    PageFault { addr: u64, code: u32 },
}

impl Exception {
    pub fn vector(&self) -> u8 {
        match self {
            Exception::DivideError => 0,
            Exception::InvalidOpcode => 6,
            Exception::GeneralProtection { .. } => 13,
            Exception::PageFault { .. } => 14,
        }
    }

    pub fn error_code(&self) -> Option<u32> {
        match self {
            Exception::GeneralProtection { code } => Some(*code),
            Exception::PageFault { code, .. } => Some(*code),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FpuState {
    pub fxsave_area: [u8; 512],
    pub mxcsr: u32,
}

impl Default for FpuState {
    fn default() -> Self {
        Self {
            fxsave_area: [0u8; 512],
            mxcsr: 0x1F80,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CpuState {
    gprs: [u64; 16],
    pub rip: u64,
    pub rflags: Rflags,

    pub es: SegmentRegister,
    pub cs: SegmentRegister,
    pub ss: SegmentRegister,
    pub ds: SegmentRegister,
    pub fs: SegmentRegister,
    pub gs: SegmentRegister,

    pub gdtr: DescriptorTableReg,
    pub idtr: DescriptorTableReg,
    pub ldtr: SystemSegmentRegister,
    pub tr: SystemSegmentRegister,

    pub control: ControlRegs,
    pub debug: DebugRegs,

    pub msrs: Msrs,

    pub fpu: FpuState,

    /// FIFO of externally injected interrupts (PIC/APIC).
    pub external_interrupts: VecDeque<u8>,
    /// Interrupt shadow after STI (inhibits interrupts for one instruction).
    pub interrupt_inhibit: u8,
    pub(crate) interrupt_frames: Vec<crate::interrupts::InterruptStackFrame>,
}

impl Default for CpuState {
    fn default() -> Self {
        let mut cpu = Self {
            gprs: [0; 16],
            rip: 0,
            rflags: Rflags::default(),
            es: SegmentRegister::null(),
            cs: SegmentRegister::null(),
            ss: SegmentRegister::null(),
            ds: SegmentRegister::null(),
            fs: SegmentRegister::null(),
            gs: SegmentRegister::null(),
            gdtr: DescriptorTableReg { base: 0, limit: 0 },
            idtr: DescriptorTableReg { base: 0, limit: 0 },
            ldtr: SystemSegmentRegister::null(),
            tr: SystemSegmentRegister::null(),
            control: ControlRegs::default(),
            debug: DebugRegs::default(),
            msrs: Msrs::default(),
            fpu: FpuState::default(),
            external_interrupts: VecDeque::new(),
            interrupt_inhibit: 0,
            interrupt_frames: Vec::new(),
        };

        cpu.set_real_mode_segment_defaults();
        cpu
    }
}

impl CpuState {
    fn set_real_mode_segment_defaults(&mut self) {
        self.cs = SegmentRegister {
            selector: 0,
            cache: SegmentCache {
                base: 0,
                limit: 0xFFFF,
                access: 0x9B, // present, code, readable
                flags: 0,
            },
        };
        self.ds = SegmentRegister {
            selector: 0,
            cache: SegmentCache {
                base: 0,
                limit: 0xFFFF,
                access: 0x93, // present, data, writable
                flags: 0,
            },
        };
        self.es = self.ds;
        self.ss = self.ds;
        self.fs = self.ds;
        self.gs = self.ds;

        self.idtr = DescriptorTableReg {
            base: 0,
            limit: 0x03FF,
        };
    }

    pub fn gpr64(&self, reg: Gpr) -> u64 {
        self.gprs[reg as usize]
    }

    pub fn set_gpr64(&mut self, reg: Gpr, val: u64) {
        self.gprs[reg as usize] = val;
    }

    pub fn gpr32(&self, reg: Gpr) -> u32 {
        self.gpr64(reg) as u32
    }

    pub fn set_gpr32(&mut self, reg: Gpr, val: u32) {
        // x86-64 semantics: 32-bit writes zero-extend.
        self.set_gpr64(reg, val as u64);
    }

    pub fn gpr16(&self, reg: Gpr) -> u16 {
        self.gpr64(reg) as u16
    }

    pub fn set_gpr16(&mut self, reg: Gpr, val: u16) {
        let old = self.gpr64(reg);
        let new = (old & !0xFFFF) | (val as u64);
        self.set_gpr64(reg, new);
    }

    pub fn gpr8l(&self, reg: Gpr) -> u8 {
        self.gpr64(reg) as u8
    }

    pub fn set_gpr8l(&mut self, reg: Gpr, val: u8) {
        let old = self.gpr64(reg);
        let new = (old & !0xFF) | (val as u64);
        self.set_gpr64(reg, new);
    }

    pub fn gpr8h(&self, reg: Gpr) -> Result<u8, Exception> {
        match reg {
            Gpr::Rax | Gpr::Rcx | Gpr::Rdx | Gpr::Rbx => Ok(((self.gpr64(reg) >> 8) & 0xFF) as u8),
            _ => Err(Exception::InvalidOpcode),
        }
    }

    pub fn set_gpr8h(&mut self, reg: Gpr, val: u8) -> Result<(), Exception> {
        match reg {
            Gpr::Rax | Gpr::Rcx | Gpr::Rdx | Gpr::Rbx => {
                let old = self.gpr64(reg);
                let new = (old & !(0xFF << 8)) | ((val as u64) << 8);
                self.set_gpr64(reg, new);
                Ok(())
            }
            _ => Err(Exception::InvalidOpcode),
        }
    }

    pub fn ip(&self) -> u16 {
        self.rip as u16
    }

    pub fn eip(&self) -> u32 {
        self.rip as u32
    }

    pub fn set_ip(&mut self, ip: u16) {
        self.rip = (self.rip & !0xFFFF) | (ip as u64);
    }

    pub fn set_eip(&mut self, eip: u32) {
        self.rip = eip as u64;
    }

    pub fn is_protected_mode(&self) -> bool {
        self.control.cr0 & CR0_PE != 0
    }

    pub fn is_real_mode(&self) -> bool {
        !self.is_protected_mode()
    }

    pub fn paging_enabled(&self) -> bool {
        self.control.cr0 & CR0_PG != 0
    }

    pub fn long_mode_active(&self) -> bool {
        self.msrs.efer & EFER_LMA != 0
    }

    pub fn long_mode_enabled(&self) -> bool {
        self.msrs.efer & EFER_LME != 0
    }

    pub fn is_64bit_mode(&self) -> bool {
        self.long_mode_active() && (self.cs.cache.flags & 0b0010 != 0)
    }

    pub fn cpl(&self) -> u8 {
        if self.is_real_mode() {
            0
        } else {
            (self.cs.selector & 0x3) as u8
        }
    }

    pub fn cli(&mut self) -> Result<(), Exception> {
        if !self.is_real_mode() && self.cpl() != 0 {
            return Err(Exception::GeneralProtection { code: 0 });
        }
        self.rflags.set_if(false);
        Ok(())
    }

    pub fn sti(&mut self) -> Result<(), Exception> {
        if !self.is_real_mode() && self.cpl() != 0 {
            return Err(Exception::GeneralProtection { code: 0 });
        }
        self.rflags.set_if(true);
        self.interrupt_inhibit = 1;
        Ok(())
    }

    pub fn retire_instruction(&mut self) {
        if self.interrupt_inhibit > 0 {
            self.interrupt_inhibit -= 1;
        }
    }

    fn is_canonical(addr: u64) -> bool {
        let sign = (addr >> 47) & 1;
        let upper = addr >> 48;
        if sign == 0 {
            upper == 0
        } else {
            upper == 0xFFFF
        }
    }

    fn load_code_segment_privilege<M: Memory>(
        &mut self,
        mem: &M,
        selector: u16,
        cpl: u8,
    ) -> Result<(), Exception> {
        let desc = self.fetch_segment_descriptor(selector, mem)?;
        if !desc.is_present() || !desc.is_code() {
            return Err(Exception::GeneralProtection {
                code: selector as u32,
            });
        }
        if desc.dpl() != cpl {
            return Err(Exception::GeneralProtection {
                code: selector as u32,
            });
        }
        if self.long_mode_active() && desc.long() && desc.default_operand_size_32() {
            return Err(Exception::GeneralProtection {
                code: selector as u32,
            });
        }

        self.cs = SegmentRegister {
            selector: (selector & !0x3) | (cpl as u16),
            cache: self.build_segment_cache(SegReg::Cs, &desc),
        };
        Ok(())
    }

    fn load_stack_segment_privilege<M: Memory>(
        &mut self,
        mem: &M,
        selector: u16,
        cpl: u8,
    ) -> Result<(), Exception> {
        let desc = self.fetch_segment_descriptor(selector, mem)?;
        if !desc.is_present() || !desc.is_data() || !desc.data_writable() {
            return Err(Exception::GeneralProtection {
                code: selector as u32,
            });
        }
        if desc.dpl() != cpl {
            return Err(Exception::GeneralProtection {
                code: selector as u32,
            });
        }
        self.ss = SegmentRegister {
            selector: (selector & !0x3) | (cpl as u16),
            cache: self.build_segment_cache(SegReg::Ss, &desc),
        };
        Ok(())
    }

    pub fn swapgs(&mut self) -> Result<(), Exception> {
        if !self.is_64bit_mode() {
            return Err(Exception::InvalidOpcode);
        }
        if self.cpl() != 0 {
            return Err(Exception::GeneralProtection { code: 0 });
        }

        core::mem::swap(&mut self.msrs.gs_base, &mut self.msrs.kernel_gs_base);
        self.gs.cache.base = self.msrs.gs_base;
        Ok(())
    }

    pub fn syscall<M: Memory>(&mut self, mem: &M) -> Result<(), Exception> {
        if !self.long_mode_active() {
            return Err(Exception::InvalidOpcode);
        }
        if (self.msrs.efer & EFER_SCE) == 0 {
            return Err(Exception::InvalidOpcode);
        }

        // SYSCALL instruction length is fixed at 2 bytes.
        let return_rip = self.rip.wrapping_add(2);
        let old_flags = self.rflags.read();

        self.set_gpr64(Gpr::Rcx, return_rip);
        self.set_gpr64(Gpr::R11, old_flags);

        let star = self.msrs.star;
        let kernel_cs = ((star >> 32) & 0xFFFF) as u16;
        let kernel_ss = kernel_cs.wrapping_add(8);

        let cs_desc = self.fetch_segment_descriptor(kernel_cs, mem)?;
        if !cs_desc.long() {
            return Err(Exception::GeneralProtection {
                code: kernel_cs as u32,
            });
        }

        self.load_code_segment_privilege(mem, kernel_cs, 0)?;
        self.load_stack_segment_privilege(mem, kernel_ss, 0)?;

        let mut new_flags = old_flags & !self.msrs.sfmask;
        // Per architectural rules, bit 1 is always set.
        new_flags |= 1 << 1;
        self.rflags.set_raw(new_flags);

        let target = self.msrs.lstar;
        if !Self::is_canonical(target) {
            return Err(Exception::GeneralProtection { code: 0 });
        }
        self.rip = target;
        Ok(())
    }

    pub fn sysret<M: Memory>(&mut self, mem: &M) -> Result<(), Exception> {
        if !self.long_mode_active() {
            return Err(Exception::InvalidOpcode);
        }
        if (self.msrs.efer & EFER_SCE) == 0 {
            return Err(Exception::InvalidOpcode);
        }
        if self.cpl() != 0 {
            return Err(Exception::GeneralProtection { code: 0 });
        }

        let target = self.gpr64(Gpr::Rcx);
        if !Self::is_canonical(target) {
            return Err(Exception::GeneralProtection { code: 0 });
        }

        let flags = self.gpr64(Gpr::R11);
        self.rflags.set_raw(flags);

        let star = self.msrs.star;
        let base = ((star >> 48) & 0xFFFF) as u16;
        let user_ss = base.wrapping_add(8);
        let user_cs = base.wrapping_add(16);

        let cs_desc = self.fetch_segment_descriptor(user_cs, mem)?;
        if !cs_desc.long() {
            return Err(Exception::GeneralProtection {
                code: user_cs as u32,
            });
        }

        self.load_stack_segment_privilege(mem, user_ss, 3)?;
        self.load_code_segment_privilege(mem, user_cs, 3)?;

        self.rip = target;
        Ok(())
    }

    pub fn sysenter<M: Memory>(&mut self, mem: &M) -> Result<(), Exception> {
        if self.is_real_mode() {
            return Err(Exception::InvalidOpcode);
        }

        let cs = self.msrs.sysenter_cs as u16;
        if cs == 0 {
            return Err(Exception::GeneralProtection { code: 0 });
        }
        let ss = cs.wrapping_add(8);

        self.load_code_segment_privilege(mem, cs, 0)?;
        self.load_stack_segment_privilege(mem, ss, 0)?;

        if self.long_mode_active() {
            let rip = self.msrs.sysenter_eip;
            let rsp = self.msrs.sysenter_esp;
            if !Self::is_canonical(rip) || !Self::is_canonical(rsp) {
                return Err(Exception::GeneralProtection { code: 0 });
            }
            self.rip = rip;
            self.set_gpr64(Gpr::Rsp, rsp);
        } else {
            self.set_eip(self.msrs.sysenter_eip as u32);
            self.set_gpr32(Gpr::Rsp, self.msrs.sysenter_esp as u32);
        }

        Ok(())
    }

    pub fn sysexit<M: Memory>(&mut self, mem: &M) -> Result<(), Exception> {
        if self.is_real_mode() {
            return Err(Exception::InvalidOpcode);
        }
        if self.cpl() != 0 {
            return Err(Exception::GeneralProtection { code: 0 });
        }

        let cs_base = self.msrs.sysenter_cs as u16;
        if cs_base == 0 {
            return Err(Exception::GeneralProtection { code: 0 });
        }

        let user_cs = cs_base.wrapping_add(16);
        let user_ss = cs_base.wrapping_add(24);

        if self.long_mode_active() {
            let rip = self.gpr64(Gpr::Rcx);
            let rsp = self.gpr64(Gpr::Rdx);
            if !Self::is_canonical(rip) || !Self::is_canonical(rsp) {
                return Err(Exception::GeneralProtection { code: 0 });
            }
            self.load_stack_segment_privilege(mem, user_ss, 3)?;
            self.load_code_segment_privilege(mem, user_cs, 3)?;
            self.rip = rip;
            self.set_gpr64(Gpr::Rsp, rsp);
        } else {
            let eip = self.gpr32(Gpr::Rcx);
            let esp = self.gpr32(Gpr::Rdx);
            self.load_stack_segment_privilege(mem, user_ss, 3)?;
            self.load_code_segment_privilege(mem, user_cs, 3)?;
            self.set_eip(eip);
            self.set_gpr32(Gpr::Rsp, esp);
        }

        Ok(())
    }

    pub fn write_cr0(&mut self, val: u64) {
        self.control.cr0 = val;
        self.recompute_long_mode_active();
    }

    pub fn write_cr2(&mut self, val: u64) {
        self.control.cr2 = val;
    }

    pub fn write_cr3(&mut self, val: u64) {
        self.control.cr3 = val;
    }

    pub fn write_cr4(&mut self, val: u64) {
        self.control.cr4 = val;
        self.recompute_long_mode_active();
    }

    pub fn write_msr(&mut self, msr: u32, val: u64) -> Result<(), MsrError> {
        self.msrs.write(msr, val)?;
        match msr {
            IA32_EFER => self.recompute_long_mode_active(),
            IA32_FS_BASE => {
                if self.long_mode_active() {
                    self.fs.cache.base = self.msrs.fs_base;
                }
            }
            IA32_GS_BASE => {
                if self.long_mode_active() {
                    self.gs.cache.base = self.msrs.gs_base;
                }
            }
            IA32_KERNEL_GS_BASE => {}
            _ => {}
        }
        Ok(())
    }

    fn recompute_long_mode_active(&mut self) {
        let lma = (self.control.cr0 & CR0_PG != 0)
            && (self.control.cr4 & CR4_PAE != 0)
            && (self.msrs.efer & EFER_LME != 0);
        if lma {
            self.msrs.efer |= EFER_LMA;
        } else {
            self.msrs.efer &= !EFER_LMA;
        }
    }

    pub fn lgdt(&mut self, base: u64, limit: u16) {
        self.gdtr = DescriptorTableReg { base, limit };
    }

    pub fn lidt(&mut self, base: u64, limit: u16) {
        self.idtr = DescriptorTableReg { base, limit };
    }

    pub fn set_segment_real_mode(&mut self, reg: SegReg, selector: u16) {
        let cache = SegmentCache {
            base: (selector as u64) << 4,
            limit: 0xFFFF,
            access: 0x93,
            flags: 0,
        };
        let seg = SegmentRegister { selector, cache };
        match reg {
            SegReg::Es => self.es = seg,
            SegReg::Cs => {
                self.cs = SegmentRegister {
                    cache: SegmentCache {
                        access: 0x9B,
                        ..cache
                    },
                    ..seg
                }
            }
            SegReg::Ss => self.ss = seg,
            SegReg::Ds => self.ds = seg,
            SegReg::Fs => self.fs = seg,
            SegReg::Gs => self.gs = seg,
        }
    }

    pub fn load_segment<M: Memory>(
        &mut self,
        reg: SegReg,
        selector: u16,
        mem: &M,
    ) -> Result<(), Exception> {
        if self.is_real_mode() {
            self.set_segment_real_mode(reg, selector);
            return Ok(());
        }

        if selector & 0xFFFC == 0 {
            // null selector
            match reg {
                SegReg::Cs | SegReg::Ss => return Err(Exception::GeneralProtection { code: 0 }),
                _ => {
                    self.set_segment(reg, SegmentRegister::null());
                    return Ok(());
                }
            }
        }

        let desc_bytes = self.fetch_descriptor_bytes(selector, mem)?;
        let desc = parse_segment_descriptor(desc_bytes);

        if !desc.is_present() {
            return Err(Exception::GeneralProtection {
                code: selector as u32,
            });
        }

        let rpl = (selector & 0x3) as u8;
        let cpl = self.cpl();
        let effective_priv = cpl.max(rpl);

        match reg {
            SegReg::Cs => {
                if !desc.is_code() {
                    return Err(Exception::GeneralProtection {
                        code: selector as u32,
                    });
                }

                if desc.code_conforming() {
                    if desc.dpl() > cpl || rpl != cpl {
                        return Err(Exception::GeneralProtection {
                            code: selector as u32,
                        });
                    }
                } else if desc.dpl() != cpl || rpl != cpl {
                    return Err(Exception::GeneralProtection {
                        code: selector as u32,
                    });
                }

                if self.long_mode_active() {
                    if desc.long() && desc.default_operand_size_32() {
                        // 64-bit code segment requires D=0.
                        return Err(Exception::GeneralProtection {
                            code: selector as u32,
                        });
                    }
                }
            }
            SegReg::Ss => {
                if !desc.is_data() || !desc.data_writable() {
                    return Err(Exception::GeneralProtection {
                        code: selector as u32,
                    });
                }
                if desc.dpl() != cpl || rpl != cpl {
                    return Err(Exception::GeneralProtection {
                        code: selector as u32,
                    });
                }
            }
            _ => {
                if !(desc.is_data() || desc.code_readable()) {
                    return Err(Exception::GeneralProtection {
                        code: selector as u32,
                    });
                }
                if desc.dpl() < effective_priv {
                    return Err(Exception::GeneralProtection {
                        code: selector as u32,
                    });
                }
            }
        }

        let cache = if self.long_mode_active() {
            // In long mode, base/limit are mostly ignored (except FS/GS base comes from MSRs).
            match reg {
                SegReg::Fs => SegmentCache {
                    base: self.msrs.fs_base,
                    limit: 0xFFFF_FFFF,
                    access: desc.access,
                    flags: desc.flags,
                },
                SegReg::Gs => SegmentCache {
                    base: self.msrs.gs_base,
                    limit: 0xFFFF_FFFF,
                    access: desc.access,
                    flags: desc.flags,
                },
                _ => SegmentCache {
                    base: 0,
                    limit: 0xFFFF_FFFF,
                    access: desc.access,
                    flags: desc.flags,
                },
            }
        } else {
            SegmentCache {
                base: desc.base,
                limit: desc.effective_limit(),
                access: desc.access,
                flags: desc.flags,
            }
        };

        let sel_with_cpl = if reg == SegReg::Cs {
            // Interrupts and far transfers in protected mode must keep CPL consistent; use current CPL.
            (selector & !0x3) | (cpl as u16)
        } else {
            selector
        };

        self.set_segment(
            reg,
            SegmentRegister {
                selector: sel_with_cpl,
                cache,
            },
        );

        Ok(())
    }

    pub(crate) fn fetch_segment_descriptor<M: Memory>(
        &self,
        selector: u16,
        mem: &M,
    ) -> Result<crate::descriptors::SegmentDescriptor, Exception> {
        let bytes = self.fetch_descriptor_bytes(selector, mem)?;
        Ok(parse_segment_descriptor(bytes))
    }

    pub(crate) fn build_segment_cache(
        &self,
        reg: SegReg,
        desc: &crate::descriptors::SegmentDescriptor,
    ) -> SegmentCache {
        if self.long_mode_active() {
            match reg {
                SegReg::Fs => SegmentCache {
                    base: self.msrs.fs_base,
                    limit: 0xFFFF_FFFF,
                    access: desc.access,
                    flags: desc.flags,
                },
                SegReg::Gs => SegmentCache {
                    base: self.msrs.gs_base,
                    limit: 0xFFFF_FFFF,
                    access: desc.access,
                    flags: desc.flags,
                },
                _ => SegmentCache {
                    base: 0,
                    limit: 0xFFFF_FFFF,
                    access: desc.access,
                    flags: desc.flags,
                },
            }
        } else {
            SegmentCache {
                base: desc.base,
                limit: desc.effective_limit(),
                access: desc.access,
                flags: desc.flags,
            }
        }
    }

    fn set_segment(&mut self, reg: SegReg, seg: SegmentRegister) {
        match reg {
            SegReg::Es => self.es = seg,
            SegReg::Cs => self.cs = seg,
            SegReg::Ss => self.ss = seg,
            SegReg::Ds => self.ds = seg,
            SegReg::Fs => self.fs = seg,
            SegReg::Gs => self.gs = seg,
        }
    }

    fn fetch_descriptor_bytes<M: Memory>(
        &self,
        selector: u16,
        mem: &M,
    ) -> Result<[u8; 8], Exception> {
        let index = (selector >> 3) as u64;
        let offset = index.checked_mul(8).ok_or(Exception::GeneralProtection {
            code: selector as u32,
        })?;
        let table = if selector & 0x4 == 0 {
            self.gdtr
        } else {
            DescriptorTableReg {
                base: self.ldtr.base,
                limit: self.ldtr.limit as u16,
            }
        };

        if !table.contains(offset, 8) {
            return Err(Exception::GeneralProtection {
                code: selector as u32,
            });
        }

        let mut bytes = [0u8; 8];
        mem.read(table.base + offset, &mut bytes)
            .map_err(|_| Exception::GeneralProtection {
                code: selector as u32,
            })?;
        Ok(bytes)
    }

    fn fetch_descriptor_16_bytes<M: Memory>(
        &self,
        selector: u16,
        mem: &M,
    ) -> Result<[u8; 16], Exception> {
        let index = (selector >> 3) as u64;
        let offset = index.checked_mul(8).ok_or(Exception::GeneralProtection {
            code: selector as u32,
        })?;
        if selector & 0x4 != 0 {
            return Err(Exception::GeneralProtection {
                code: selector as u32,
            });
        }
        if !self.gdtr.contains(offset, 16) {
            return Err(Exception::GeneralProtection {
                code: selector as u32,
            });
        }

        let mut bytes = [0u8; 16];
        mem.read(self.gdtr.base + offset, &mut bytes).map_err(|_| {
            Exception::GeneralProtection {
                code: selector as u32,
            }
        })?;
        Ok(bytes)
    }

    pub fn ltr<M: Memory>(&mut self, selector: u16, mem: &M) -> Result<(), Exception> {
        if selector & 0xFFFC == 0 {
            return Err(Exception::GeneralProtection { code: 0 });
        }
        let desc = if self.long_mode_active() {
            let bytes = self.fetch_descriptor_16_bytes(selector, mem)?;
            parse_system_descriptor_64(bytes)
        } else {
            let bytes = self.fetch_descriptor_bytes(selector, mem)?;
            parse_system_descriptor(bytes)
        };

        if !desc.is_present() {
            return Err(Exception::GeneralProtection {
                code: selector as u32,
            });
        }

        // 0x9/0xB are available/busy 32-bit TSS; 0x2 is LDT; 0x9/0xB are also used.
        // For long mode: 0x9/0xB are 64-bit TSS.
        let ty = desc.system_type();
        if ty != 0x9 && ty != 0xB {
            return Err(Exception::GeneralProtection {
                code: selector as u32,
            });
        }

        self.tr = SystemSegmentRegister {
            selector,
            base: desc.base,
            limit: desc.effective_limit(),
            access: desc.access,
            flags: desc.flags,
        };

        Ok(())
    }

    pub fn current_stack_pointer(&self) -> u64 {
        self.gpr64(Gpr::Rsp)
    }

    pub fn set_stack_pointer(&mut self, val: u64) {
        self.set_gpr64(Gpr::Rsp, val);
    }

    pub fn far_jump<M: Memory>(
        &mut self,
        mem: &M,
        selector: u16,
        offset: u64,
    ) -> Result<(), Exception> {
        if self.is_real_mode() {
            self.set_segment_real_mode(SegReg::Cs, selector);
            self.set_ip(offset as u16);
            return Ok(());
        }

        self.load_segment(SegReg::Cs, selector, mem)?;

        let mask = if self.long_mode_active() {
            if self.cs.cache.flags & 0b0010 != 0 {
                u64::MAX
            } else if self.cs.cache.flags & 0b0100 != 0 {
                0xFFFF_FFFF
            } else {
                0xFFFF
            }
        } else if self.cs.cache.flags & 0b0100 != 0 {
            0xFFFF_FFFF
        } else {
            0xFFFF
        };

        self.rip = offset & mask;
        Ok(())
    }

    pub fn far_call_real_mode<M: Memory>(
        &mut self,
        mem: &mut M,
        selector: u16,
        offset: u16,
    ) -> Result<(), Exception> {
        if !self.is_real_mode() {
            return Err(Exception::InvalidOpcode);
        }

        let old_cs = self.cs.selector;
        let old_ip = self.ip();

        let mut sp = self.gpr16(Gpr::Rsp);
        sp = sp.wrapping_sub(2);
        mem.write_u16(self.ss.cache.base + sp as u64, old_cs)
            .map_err(|_| Exception::GeneralProtection { code: 0 })?;
        sp = sp.wrapping_sub(2);
        mem.write_u16(self.ss.cache.base + sp as u64, old_ip)
            .map_err(|_| Exception::GeneralProtection { code: 0 })?;
        self.set_gpr16(Gpr::Rsp, sp);

        self.set_segment_real_mode(SegReg::Cs, selector);
        self.set_ip(offset);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aero_core::memory::VecMemory;

    fn write_desc(mem: &mut VecMemory, addr: u64, desc: u64) {
        mem.write_u64(addr, desc).unwrap();
    }

    #[test]
    fn segment_descriptor_parsing_base_limit_and_granularity() {
        // Base = 0x12345678, limit (20-bit) = 0x9009A, G=1 (4k pages).
        let bytes = [
            0x9A, 0x00, // limit low = 0x009A
            0x78, 0x56, // base low
            0x34, // base mid
            0x92, // access
            0xB9, // flags(0xB) + limit high (0x9)
            0x12, // base high
        ];

        let parsed = parse_segment_descriptor(bytes);
        assert_eq!(parsed.base, 0x12345678);
        assert_eq!(parsed.limit, 0x9_009A & 0xFFFFF); // 20-bit value
        assert!(parsed.granularity_4k());
        assert_eq!(parsed.effective_limit(), (parsed.limit << 12) | 0xFFF);
    }

    #[test]
    fn load_segment_null_selector_rules() {
        let mut mem = VecMemory::new(0x2000);
        let mut cpu = CpuState::default();

        // protected mode with a minimal GDT
        let gdt_base = 0x1000;
        write_desc(&mut mem, gdt_base, 0);
        write_desc(&mut mem, gdt_base + 8, 0x00CF9A000000FFFF);
        write_desc(&mut mem, gdt_base + 16, 0x00CF92000000FFFF);
        cpu.lgdt(gdt_base, 0x17);
        cpu.write_cr0(cpu.control.cr0 | CR0_PE);
        cpu.load_segment(SegReg::Cs, 0x08, &mem).unwrap();

        // DS may be loaded with null selector.
        cpu.load_segment(SegReg::Ds, 0x0000, &mem).unwrap();
        assert_eq!(cpu.ds.selector, 0);
        assert_eq!(cpu.ds.cache, SegmentCache::unusable());

        // SS may not be loaded with null selector.
        assert!(matches!(
            cpu.load_segment(SegReg::Ss, 0x0000, &mem),
            Err(Exception::GeneralProtection { .. })
        ));
    }

    #[test]
    fn load_segment_privilege_checks_rpl_for_data_segments() {
        let mut mem = VecMemory::new(0x2000);
        let mut cpu = CpuState::default();

        let gdt_base = 0x1000;
        write_desc(&mut mem, gdt_base, 0);
        write_desc(&mut mem, gdt_base + 8, 0x00CF9A000000FFFF); // ring0 code
        write_desc(&mut mem, gdt_base + 16, 0x00CF92000000FFFF); // ring0 data
        cpu.lgdt(gdt_base, 0x17);
        cpu.write_cr0(cpu.control.cr0 | CR0_PE);
        cpu.load_segment(SegReg::Cs, 0x08, &mem).unwrap(); // CPL=0

        // Loading DS with selector RPL=3 raises #GP when DPL=0.
        let err = cpu.load_segment(SegReg::Ds, 0x10 | 3, &mem).unwrap_err();
        assert!(matches!(err, Exception::GeneralProtection { .. }));
    }

    #[test]
    fn long_mode_fs_base_comes_from_msr() {
        let mut mem = VecMemory::new(0x4000);
        let mut cpu = CpuState::default();

        // GDT: null, 64-bit code, data, fs descriptor with non-zero base.
        let gdt_base = 0x1000;
        write_desc(&mut mem, gdt_base, 0);
        write_desc(&mut mem, gdt_base + 8, 0x00AF9A000000FFFF);
        write_desc(&mut mem, gdt_base + 16, 0x00CF92000000FFFF);
        write_desc(&mut mem, gdt_base + 24, 0x00CF9200123400FF); // base=0x12340000 (ignored)
        cpu.lgdt(gdt_base, 0x1F);

        cpu.write_cr0(cpu.control.cr0 | CR0_PE);
        cpu.write_cr4(cpu.control.cr4 | CR4_PAE);
        cpu.write_msr(IA32_EFER, cpu.msrs.efer | EFER_LME).unwrap();
        cpu.write_cr0(cpu.control.cr0 | CR0_PG);

        cpu.write_msr(IA32_FS_BASE, 0xDEAD_BEEF_CAFE_BABE).unwrap();
        cpu.load_segment(SegReg::Fs, 0x18, &mem).unwrap();
        assert_eq!(cpu.fs.cache.base, 0xDEAD_BEEF_CAFE_BABE);
    }

    #[test]
    fn real_mode_far_call_pushes_return_cs_ip() {
        let mut mem = VecMemory::new(0x40000);
        let mut cpu = CpuState::default();
        cpu.set_segment_real_mode(SegReg::Cs, 0x0000);
        cpu.set_segment_real_mode(SegReg::Ss, 0x2000);
        cpu.set_ip(0x0100);
        cpu.set_gpr16(Gpr::Rsp, 0xFFFE);

        cpu.far_call_real_mode(&mut mem, 0x1000, 0x0000).unwrap();

        assert_eq!(cpu.cs.selector, 0x1000);
        assert_eq!(cpu.cs.cache.base, 0x1000_0);
        assert_eq!(cpu.ip(), 0x0000);

        let sp = cpu.gpr16(Gpr::Rsp);
        assert_eq!(sp, 0xFFFA);
        let stack_base = cpu.ss.cache.base + sp as u64;
        let ret_ip = mem.read_u16(stack_base).unwrap();
        let ret_cs = mem.read_u16(stack_base + 2).unwrap();
        assert_eq!(ret_ip, 0x0100);
        assert_eq!(ret_cs, 0x0000);
    }

    #[test]
    fn protected_mode_far_jump_loads_cs_and_eip() {
        let mut mem = VecMemory::new(0x4000);
        let mut cpu = CpuState::default();

        let gdt_base = 0x1000;
        write_desc(&mut mem, gdt_base, 0);
        write_desc(&mut mem, gdt_base + 8, 0x00CF9A000000FFFF);
        write_desc(&mut mem, gdt_base + 16, 0x00CF92000000FFFF);
        cpu.lgdt(gdt_base, 0x17);

        cpu.write_cr0(cpu.control.cr0 | CR0_PE);
        cpu.far_jump(&mem, 0x08, 0x1234).unwrap();
        assert!(cpu.is_protected_mode());
        assert_eq!(cpu.cs.selector, 0x08);
        assert_eq!(cpu.rip, 0x1234);
    }

    #[test]
    fn long_mode_transition_sets_lma_and_validates_64bit_cs() {
        let mut mem = VecMemory::new(0x4000);
        let mut cpu = CpuState::default();

        // GDT: null, 64-bit code, data.
        let gdt_base = 0x1000;
        write_desc(&mut mem, gdt_base, 0);
        write_desc(&mut mem, gdt_base + 8, 0x00AF9A000000FFFF);
        write_desc(&mut mem, gdt_base + 16, 0x00CF92000000FFFF);
        cpu.lgdt(gdt_base, 0x17);

        cpu.write_cr0(cpu.control.cr0 | CR0_PE);
        cpu.write_cr4(cpu.control.cr4 | CR4_PAE);
        cpu.write_msr(IA32_EFER, cpu.msrs.efer | EFER_LME).unwrap();
        cpu.write_cr0(cpu.control.cr0 | CR0_PG);

        assert!(cpu.long_mode_active());

        // Load a 64-bit CS; should enable 64-bit submode.
        cpu.load_segment(SegReg::Cs, 0x08, &mem).unwrap();
        assert!(cpu.is_64bit_mode());

        // Reject a code descriptor with L=1 and D=1.
        write_desc(&mut mem, gdt_base + 24, 0x00EF9A000000FFFF);
        assert!(matches!(
            cpu.load_segment(SegReg::Cs, 0x18, &mem),
            Err(Exception::GeneralProtection { .. })
        ));
    }

    #[test]
    fn syscall_sysret_transitions_privilege() {
        let mut mem = VecMemory::new(0x8000);
        let mut cpu = CpuState::default();

        // GDT: null, kernel 64-bit code, kernel data, user data, user 64-bit code.
        let gdt_base = 0x1000;
        write_desc(&mut mem, gdt_base, 0);
        write_desc(&mut mem, gdt_base + 8, 0x00AF9A000000FFFF);
        write_desc(&mut mem, gdt_base + 16, 0x00CF92000000FFFF);
        write_desc(&mut mem, gdt_base + 24, 0x00CFF2000000FFFF);
        write_desc(&mut mem, gdt_base + 32, 0x00AFFA000000FFFF);
        cpu.lgdt(gdt_base, 0x27);

        cpu.write_cr0(cpu.control.cr0 | CR0_PE);
        cpu.write_cr4(cpu.control.cr4 | CR4_PAE);
        cpu.write_msr(IA32_EFER, cpu.msrs.efer | EFER_LME | EFER_SCE)
            .unwrap();
        cpu.write_cr0(cpu.control.cr0 | CR0_PG);
        assert!(cpu.long_mode_active());

        // Start in user mode.
        cpu.cs.selector = 0x23;
        cpu.load_segment(SegReg::Cs, 0x23, &mem).unwrap();
        cpu.load_segment(SegReg::Ss, 0x1B, &mem).unwrap();
        cpu.rip = 0x1000;
        cpu.set_gpr64(Gpr::Rsp, 0x7000);
        cpu.rflags.set_raw((1 << 1) | RFLAGS_IF);

        // Configure syscall MSRs.
        cpu.msrs.star = ((0x08u64) << 32) | ((0x10u64) << 48);
        cpu.msrs.lstar = 0xFFFF_8000_0000_0000;
        cpu.msrs.sfmask = RFLAGS_IF;

        cpu.syscall(&mem).unwrap();
        assert_eq!(cpu.cpl(), 0);
        assert_eq!(cpu.cs.selector, 0x08);
        assert_eq!(cpu.ss.selector, 0x10);
        assert_eq!(cpu.rip, cpu.msrs.lstar);
        assert_eq!(cpu.gpr64(Gpr::Rcx), 0x1002);
        assert_eq!(cpu.gpr64(Gpr::R11), (1 << 1) | RFLAGS_IF);
        assert!(!cpu.rflags.if_flag());

        // Return to user.
        cpu.set_gpr64(Gpr::Rcx, 0x2000);
        cpu.set_gpr64(Gpr::R11, (1 << 1) | RFLAGS_IF);
        cpu.sysret(&mem).unwrap();
        assert_eq!(cpu.cpl(), 3);
        assert_eq!(cpu.cs.selector, 0x23);
        assert_eq!(cpu.ss.selector, 0x1B);
        assert_eq!(cpu.rip, 0x2000);
        assert!(cpu.rflags.if_flag());
    }

    #[test]
    fn sysenter_sysexit_transitions_32bit() {
        let mut mem = VecMemory::new(0x8000);
        let mut cpu = CpuState::default();

        // GDT: null, kernel code/data, user code/data.
        let gdt_base = 0x1000;
        write_desc(&mut mem, gdt_base, 0);
        write_desc(&mut mem, gdt_base + 8, 0x00CF9A000000FFFF);
        write_desc(&mut mem, gdt_base + 16, 0x00CF92000000FFFF);
        write_desc(&mut mem, gdt_base + 24, 0x00CFFA000000FFFF);
        write_desc(&mut mem, gdt_base + 32, 0x00CFF2000000FFFF);
        cpu.lgdt(gdt_base, 0x27);

        cpu.write_cr0(cpu.control.cr0 | CR0_PE);

        // Start in user mode.
        cpu.cs.selector = 0x1B;
        cpu.load_segment(SegReg::Cs, 0x1B, &mem).unwrap();
        cpu.load_segment(SegReg::Ss, 0x23, &mem).unwrap();

        cpu.msrs.sysenter_cs = 0x08;
        cpu.msrs.sysenter_eip = 0xC000_1000;
        cpu.msrs.sysenter_esp = 0xC000_2000;

        cpu.sysenter(&mem).unwrap();
        assert_eq!(cpu.cpl(), 0);
        assert_eq!(cpu.cs.selector, 0x08);
        assert_eq!(cpu.ss.selector, 0x10);
        assert_eq!(cpu.eip(), 0xC000_1000);
        assert_eq!(cpu.gpr32(Gpr::Rsp), 0xC000_2000);

        cpu.set_gpr32(Gpr::Rcx, 0xB800_0000);
        cpu.set_gpr32(Gpr::Rdx, 0x0012_3400);
        cpu.sysexit(&mem).unwrap();

        assert_eq!(cpu.cpl(), 3);
        assert_eq!(cpu.cs.selector, 0x1B);
        assert_eq!(cpu.ss.selector, 0x23);
        assert_eq!(cpu.eip(), 0xB800_0000);
        assert_eq!(cpu.gpr32(Gpr::Rsp), 0x0012_3400);
    }

    #[test]
    fn swapgs_swaps_kernel_and_user_gs_base() {
        let mut mem = VecMemory::new(0x4000);
        let mut cpu = CpuState::default();

        let gdt_base = 0x1000;
        write_desc(&mut mem, gdt_base, 0);
        write_desc(&mut mem, gdt_base + 8, 0x00AF9A000000FFFF);
        write_desc(&mut mem, gdt_base + 16, 0x00CF92000000FFFF);
        cpu.lgdt(gdt_base, 0x17);

        cpu.write_cr0(cpu.control.cr0 | CR0_PE);
        cpu.write_cr4(cpu.control.cr4 | CR4_PAE);
        cpu.write_msr(IA32_EFER, cpu.msrs.efer | EFER_LME).unwrap();
        cpu.write_cr0(cpu.control.cr0 | CR0_PG);
        cpu.load_segment(SegReg::Cs, 0x08, &mem).unwrap();

        cpu.write_msr(IA32_GS_BASE, 0x1111).unwrap();
        cpu.write_msr(IA32_KERNEL_GS_BASE, 0x2222).unwrap();
        assert_eq!(cpu.gs.cache.base, 0x1111);

        cpu.swapgs().unwrap();
        assert_eq!(cpu.msrs.gs_base, 0x2222);
        assert_eq!(cpu.msrs.kernel_gs_base, 0x1111);
        assert_eq!(cpu.gs.cache.base, 0x2222);
    }
}
