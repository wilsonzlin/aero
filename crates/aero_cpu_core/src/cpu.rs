use crate::bus::Bus;
use crate::cpuid::CpuFeatures;
use crate::interp::{decode, win7_ext, ExecError};
use crate::sse_state::SseState;

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
            CpuMode::Long64 => self.rax = value as u64, // zero-extend
            _ => self.rax = (self.rax & !0xFFFF_FFFF) | value as u64,
        }
    }

    pub fn set_rax(&mut self, value: u64) {
        self.rax = value;
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
}

#[derive(Clone, Debug, Default)]
pub struct Cpu {
    pub mode: CpuMode,
    pub regs: Regs,
    pub rflags: RFlags,
    pub segs: Segments,
    pub sse: SseState,
    pub features: CpuFeatures,
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
        match decode::decode(self.mode, bytes) {
            Ok(inst) => {
                crate::interp::exec(self, bus, &inst)?;
                Ok(inst.len)
            }
            Err(ExecError::InvalidOpcode(_)) => win7_ext::exec(self, bus, bytes),
            Err(e) => Err(e),
        }
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
