use crate::bus::Bus;
use crate::cpuid::CpuFeatures;
use crate::interp::{decode, win7_ext, ExecError};
use crate::sse_state::SseState;
use aero_perf::PerfWorker;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CpuMode {
    Real16,
    Protected32,
    Long64,
}

impl Default for CpuMode {
    fn default() -> Self {
        Self::Real16
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Segment {
    Cs,
    Ds,
    Es,
    Ss,
    Fs,
    Gs,
}

#[derive(Clone, Debug)]
pub struct SegmentReg {
    pub base: u64,
}

impl Default for SegmentReg {
    fn default() -> Self {
        Self { base: 0 }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Segments {
    pub cs: SegmentReg,
    pub ds: SegmentReg,
    pub es: SegmentReg,
    pub ss: SegmentReg,
    pub fs: SegmentReg,
    pub gs: SegmentReg,
}

impl Segments {
    pub fn get(&self, seg: Segment) -> &SegmentReg {
        match seg {
            Segment::Cs => &self.cs,
            Segment::Ds => &self.ds,
            Segment::Es => &self.es,
            Segment::Ss => &self.ss,
            Segment::Fs => &self.fs,
            Segment::Gs => &self.gs,
        }
    }

    pub fn get_mut(&mut self, seg: Segment) -> &mut SegmentReg {
        match seg {
            Segment::Cs => &mut self.cs,
            Segment::Ds => &mut self.ds,
            Segment::Es => &mut self.es,
            Segment::Ss => &mut self.ss,
            Segment::Fs => &mut self.fs,
            Segment::Gs => &mut self.gs,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct RFlags {
    bits: u64,
}

impl RFlags {
    pub const CF: u64 = 1 << 0;
    pub const PF: u64 = 1 << 2;
    pub const AF: u64 = 1 << 4;
    pub const ZF: u64 = 1 << 6;
    pub const SF: u64 = 1 << 7;
    pub const IF: u64 = 1 << 9;
    pub const DF: u64 = 1 << 10;
    pub const OF: u64 = 1 << 11;

    pub fn bits(&self) -> u64 {
        self.bits
    }

    pub fn get(&self, mask: u64) -> bool {
        (self.bits & mask) != 0
    }

    pub fn set(&mut self, mask: u64, value: bool) {
        if value {
            self.bits |= mask;
        } else {
            self.bits &= !mask;
        }
    }

    pub fn zf(&self) -> bool {
        self.get(Self::ZF)
    }

    pub fn set_zf(&mut self, value: bool) {
        self.set(Self::ZF, value);
    }

    pub fn df(&self) -> bool {
        self.get(Self::DF)
    }

    pub fn set_df(&mut self, value: bool) {
        self.set(Self::DF, value);
    }
}

#[derive(Clone, Debug, Default)]
pub struct Regs {
    pub rax: u64,
    pub rcx: u64,
    pub rdx: u64,
    pub rbx: u64,
    pub rsp: u64,
    pub rbp: u64,
    pub rsi: u64,
    pub rdi: u64,
    pub r8: u64,
    pub r9: u64,
    pub r10: u64,
    pub r11: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,
}

impl Regs {
    pub fn gpr(&self, index: u8) -> u64 {
        match index & 0xF {
            0 => self.rax,
            1 => self.rcx,
            2 => self.rdx,
            3 => self.rbx,
            4 => self.rsp,
            5 => self.rbp,
            6 => self.rsi,
            7 => self.rdi,
            8 => self.r8,
            9 => self.r9,
            10 => self.r10,
            11 => self.r11,
            12 => self.r12,
            13 => self.r13,
            14 => self.r14,
            15 => self.r15,
            _ => unreachable!(),
        }
    }

    pub fn set_gpr(&mut self, index: u8, value: u64) {
        match index & 0xF {
            0 => self.rax = value,
            1 => self.rcx = value,
            2 => self.rdx = value,
            3 => self.rbx = value,
            4 => self.rsp = value,
            5 => self.rbp = value,
            6 => self.rsi = value,
            7 => self.rdi = value,
            8 => self.r8 = value,
            9 => self.r9 = value,
            10 => self.r10 = value,
            11 => self.r11 = value,
            12 => self.r12 = value,
            13 => self.r13 = value,
            14 => self.r14 = value,
            15 => self.r15 = value,
            _ => unreachable!(),
        }
    }

    pub fn al(&self) -> u8 {
        self.rax as u8
    }

    pub fn ax(&self) -> u16 {
        self.rax as u16
    }

    pub fn eax(&self) -> u32 {
        self.rax as u32
    }

    pub fn set_al(&mut self, value: u8) {
        self.rax = (self.rax & !0xFF) | value as u64;
    }

    pub fn set_ax(&mut self, value: u16) {
        self.rax = (self.rax & !0xFFFF) | value as u64;
    }

    pub fn set_eax(&mut self, value: u32, mode: CpuMode) {
        match mode {
            CpuMode::Long64 => self.rax = value as u64,
            _ => self.rax = (self.rax & !0xFFFF_FFFF) | value as u64,
        }
    }

    pub fn set_rax(&mut self, value: u64) {
        self.rax = value;
    }

    pub fn dx(&self) -> u16 {
        self.rdx as u16
    }

    pub fn edx(&self) -> u32 {
        self.rdx as u32
    }

    pub fn set_dx(&mut self, value: u16) {
        self.rdx = (self.rdx & !0xFFFF) | value as u64;
    }

    pub fn set_edx(&mut self, value: u32, mode: CpuMode) {
        match mode {
            CpuMode::Long64 => self.rdx = value as u64,
            _ => self.rdx = (self.rdx & !0xFFFF_FFFF) | value as u64,
        }
    }

    pub fn set_rdx(&mut self, value: u64) {
        self.rdx = value;
    }

    pub fn bx(&self) -> u16 {
        self.rbx as u16
    }

    pub fn ebx(&self) -> u32 {
        self.rbx as u32
    }

    pub fn set_bx(&mut self, value: u16) {
        self.rbx = (self.rbx & !0xFFFF) | value as u64;
    }

    pub fn set_ebx(&mut self, value: u32, mode: CpuMode) {
        match mode {
            CpuMode::Long64 => self.rbx = value as u64,
            _ => self.rbx = (self.rbx & !0xFFFF_FFFF) | value as u64,
        }
    }

    pub fn set_rbx(&mut self, value: u64) {
        self.rbx = value;
    }

    pub fn cx(&self) -> u16 {
        self.rcx as u16
    }

    pub fn ecx(&self) -> u32 {
        self.rcx as u32
    }

    pub fn set_cx(&mut self, value: u16) {
        self.rcx = (self.rcx & !0xFFFF) | value as u64;
    }

    pub fn set_ecx(&mut self, value: u32, mode: CpuMode) {
        match mode {
            CpuMode::Long64 => self.rcx = value as u64,
            _ => self.rcx = (self.rcx & !0xFFFF_FFFF) | value as u64,
        }
    }

    pub fn set_rcx(&mut self, value: u64) {
        self.rcx = value;
    }

    pub fn si(&self) -> u16 {
        self.rsi as u16
    }

    pub fn esi(&self) -> u32 {
        self.rsi as u32
    }

    pub fn set_si(&mut self, value: u16) {
        self.rsi = (self.rsi & !0xFFFF) | value as u64;
    }

    pub fn set_esi(&mut self, value: u32, mode: CpuMode) {
        match mode {
            CpuMode::Long64 => self.rsi = value as u64,
            _ => self.rsi = (self.rsi & !0xFFFF_FFFF) | value as u64,
        }
    }

    pub fn set_rsi(&mut self, value: u64) {
        self.rsi = value;
    }

    pub fn di(&self) -> u16 {
        self.rdi as u16
    }

    pub fn edi(&self) -> u32 {
        self.rdi as u32
    }

    pub fn set_di(&mut self, value: u16) {
        self.rdi = (self.rdi & !0xFFFF) | value as u64;
    }

    pub fn set_edi(&mut self, value: u32, mode: CpuMode) {
        match mode {
            CpuMode::Long64 => self.rdi = value as u64,
            _ => self.rdi = (self.rdi & !0xFFFF_FFFF) | value as u64,
        }
    }

    pub fn set_rdi(&mut self, value: u64) {
        self.rdi = value;
    }

    fn reg64(&self, reg: u8) -> u64 {
        self.gpr(reg)
    }

    fn reg64_mut(&mut self, reg: u8) -> &mut u64 {
        match reg & 0xF {
            0 => &mut self.rax,
            1 => &mut self.rcx,
            2 => &mut self.rdx,
            3 => &mut self.rbx,
            4 => &mut self.rsp,
            5 => &mut self.rbp,
            6 => &mut self.rsi,
            7 => &mut self.rdi,
            8 => &mut self.r8,
            9 => &mut self.r9,
            10 => &mut self.r10,
            11 => &mut self.r11,
            12 => &mut self.r12,
            13 => &mut self.r13,
            14 => &mut self.r14,
            _ => &mut self.r15,
        }
    }

    pub fn get(&self, reg: u8, size: usize, rex_present: bool) -> u64 {
        match size {
            1 => self.get8(reg, rex_present) as u64,
            2 => self.reg64(reg) as u16 as u64,
            4 => self.reg64(reg) as u32 as u64,
            8 => self.reg64(reg),
            _ => self.reg64(reg),
        }
    }

    pub fn set(&mut self, reg: u8, size: usize, rex_present: bool, value: u64, mode: CpuMode) {
        match size {
            1 => self.set8(reg, rex_present, value as u8),
            2 => {
                let r = self.reg64_mut(reg);
                *r = (*r & !0xFFFF) | (value as u16 as u64);
            }
            4 => {
                let r = self.reg64_mut(reg);
                let v = value as u32 as u64;
                match mode {
                    CpuMode::Long64 => *r = v,
                    _ => *r = (*r & !0xFFFF_FFFF) | v,
                }
            }
            8 => *self.reg64_mut(reg) = value,
            _ => *self.reg64_mut(reg) = value,
        }
    }

    fn get8(&self, reg: u8, rex_present: bool) -> u8 {
        if rex_present || (reg & 0xF) < 4 || (reg & 0xF) >= 8 {
            self.reg64(reg) as u8
        } else {
            // AH/CH/DH/BH encoding.
            (self.reg64(reg - 4) >> 8) as u8
        }
    }

    fn set8(&mut self, reg: u8, rex_present: bool, value: u8) {
        if rex_present || (reg & 0xF) < 4 || (reg & 0xF) >= 8 {
            let r = self.reg64_mut(reg);
            *r = (*r & !0xFF) | value as u64;
        } else {
            let r = self.reg64_mut(reg - 4);
            *r = (*r & !0xFF00) | ((value as u64) << 8);
        }
    }
}

#[derive(Clone, Debug)]
pub struct InterruptLine(Rc<Cell<bool>>);

impl Default for InterruptLine {
    fn default() -> Self {
        Self(Rc::new(Cell::new(false)))
    }
}

impl InterruptLine {
    pub fn raise(&self) {
        self.0.set(true);
    }

    fn take(&self) -> bool {
        let was = self.0.get();
        self.0.set(false);
        was
    }
}

#[derive(Clone, Debug, Default)]
pub struct Cpu {
    pub mode: CpuMode,
    pub regs: Regs,
    pub rflags: RFlags,
    pub segs: Segments,
    pub sse: SseState,
    pub features: CpuFeatures,
    pub rip: u64,
    interrupt_line: InterruptLine,
    interrupt_inhibit_depth: u32,
    interrupts_delivered: u64,
    event_log: Option<Rc<RefCell<Vec<&'static str>>>>,
}

impl Cpu {
    pub fn new(mode: CpuMode) -> Self {
        Self {
            mode,
            ..Default::default()
        }
    }

    /// Execute a single instruction provided as bytes.
    ///
    /// This helper is sufficient for unit tests (no instruction fetch pipeline).
    pub fn execute_bytes<B: Bus>(&mut self, bus: &mut B, bytes: &[u8]) -> Result<usize, ExecError> {
        let len = match decode::decode(self.mode, bytes) {
            Ok(inst) => {
                crate::interp::exec(self, bus, &inst)?;
                inst.len
            }
            Err(ExecError::InvalidOpcode(_)) => win7_ext::exec(self, bus, bytes)?,
            Err(e) => return Err(e),
        };
        self.maybe_deliver_interrupts();
        Ok(len)
    }

    pub fn interrupt_line(&self) -> InterruptLine {
        self.interrupt_line.clone()
    }

    pub fn interrupts_delivered(&self) -> u64 {
        self.interrupts_delivered
    }

    pub fn set_event_log(&mut self, log: Rc<RefCell<Vec<&'static str>>>) {
        self.event_log = Some(log);
    }

    pub(crate) fn log_event(&mut self, evt: &'static str) {
        if let Some(log) = &self.event_log {
            log.borrow_mut().push(evt);
        }
    }

    /// Execute a single decoded guest instruction while updating performance counters.
    ///
    /// This counts one retired *architectural* instruction on success, and (for
    /// `REP*` string instructions) records the number of element-iterations
    /// executed as `rep_iterations`.
    pub fn execute_bytes_counted<B: Bus>(
        &mut self,
        bus: &mut B,
        bytes: &[u8],
        perf: &mut PerfWorker,
    ) -> Result<usize, ExecError> {
        let len = match decode::decode(self.mode, bytes) {
            Ok(inst) => {
                // For string instructions, derive REP iteration count by observing the
                // architectural count register before/after execution. This keeps the
                // hot instruction bodies free of perf/telemetry dependencies.
                let rep_count_before = match &inst.kind {
                    crate::interp::InstKind::String(s)
                        if s.prefixes.rep != crate::interp::string::RepPrefix::None =>
                    {
                        let addr_size =
                            crate::interp::string::effective_addr_size(self.mode, &s.prefixes);
                        Some((
                            addr_size,
                            crate::interp::string::read_count(self, addr_size),
                        ))
                    }
                    _ => None,
                };

                crate::interp::exec(self, bus, &inst)?;

                perf.retire_instructions(1);

                if let Some((addr_size, before)) = rep_count_before {
                    let after = crate::interp::string::read_count(self, addr_size);
                    let delta = before.saturating_sub(after);
                    if delta != 0 {
                        perf.add_rep_iterations(delta);
                    }
                }

                inst.len
            }
            Err(ExecError::InvalidOpcode(_)) => {
                let len = win7_ext::exec(self, bus, bytes)?;
                perf.retire_instructions(1);
                len
            }
            Err(e) => return Err(e),
        };
        self.maybe_deliver_interrupts();
        Ok(len)
    }

    pub(crate) fn maybe_deliver_interrupts(&mut self) {
        if self.interrupt_inhibit_depth != 0 {
            return;
        }
        if !self.rflags.get(RFlags::IF) {
            return;
        }
        if self.interrupt_line.take() {
            self.interrupts_delivered = self.interrupts_delivered.wrapping_add(1);
            self.log_event("interrupt_delivered");
        }
    }

    pub(crate) fn begin_atomic(&mut self) {
        self.interrupt_inhibit_depth = self.interrupt_inhibit_depth.wrapping_add(1);
    }

    pub(crate) fn end_atomic(&mut self) {
        self.interrupt_inhibit_depth = self.interrupt_inhibit_depth.wrapping_sub(1);
    }
    pub fn seg_base(&self, seg: Segment) -> u64 {
        match self.mode {
            CpuMode::Long64 => match seg {
                Segment::Fs => self.segs.fs.base,
                Segment::Gs => self.segs.gs.base,
                _ => 0,
            },
            _ => self.segs.get(seg).base,
        }
    }
}
