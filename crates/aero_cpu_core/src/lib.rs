//! Core architectural CPU state and privileged instruction helpers used by Aero.
//!
//! This crate serves multiple purposes:
//! - x87/SSE context save/restore (`FXSAVE`/`FXRSTOR`) used by Windows for thread switching.
//! - Privileged/system instruction surface (CPUID/MSR/SYSCALL/SYSENTER/IN/OUT) required by
//!   Windows 7 boot and kernel runtime.
//! - Interpreter helpers used by unit tests (string ops + REP semantics).

mod exception;

pub mod bus;
pub mod cpu;
pub mod cpuid;
pub mod exceptions;
pub mod exec;
pub mod fpu;
pub mod interp;
pub mod jit;
pub mod interrupts;
pub mod mem;
pub mod msr;
pub mod sse_state;
pub mod state;
pub mod system;
pub mod time;
pub mod time_insn;

pub use exception::{AssistReason, Exception};

pub use bus::{Bus, RamBus};
pub use cpu::{Cpu, CpuMode, Segment};

use crate::fpu::{canonicalize_st, FpuState};
use crate::sse_state::{SseState, MXCSR_MASK};

/// The architectural size of the FXSAVE/FXRSTOR memory image.
pub const FXSAVE_AREA_SIZE: usize = 512;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpuState {
    pub fpu: FpuState,
    pub sse: SseState,
}

impl Default for CpuState {
    fn default() -> Self {
        Self {
            fpu: FpuState::default(),
            sse: SseState::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FxStateError {
    /// Attempted to load an MXCSR value with reserved bits set.
    ///
    /// On real hardware this would raise a #GP(0).
    MxcsrReservedBits { value: u32, mask: u32 },
}

impl CpuState {
    /// Implements `FNINIT` / `FINIT`.
    pub fn fninit(&mut self) {
        self.fpu.reset();
    }

    /// Implements `EMMS` (empty MMX state).
    ///
    /// We don't currently model MMX separately from x87, but the architectural
    /// effect that matters for context switching is that the x87 tag word is
    /// marked empty.
    pub fn emms(&mut self) {
        self.fpu.emms();
    }

    /// Implements `STMXCSR m32`.
    pub fn stmxcsr(&self, dst: &mut [u8; 4]) {
        dst.copy_from_slice(&self.sse.mxcsr.to_le_bytes());
    }

    /// Implements `LDMXCSR m32`.
    pub fn ldmxcsr(&mut self, src: &[u8; 4]) -> Result<(), FxStateError> {
        self.sse.set_mxcsr(u32::from_le_bytes(*src))
    }

    /// Implements the legacy (32-bit) `FXSAVE m512byte` memory image.
    pub fn fxsave(&self, dst: &mut [u8; FXSAVE_AREA_SIZE]) {
        let mut out = [0u8; FXSAVE_AREA_SIZE];

        // 0x00..0x20: x87 environment + MXCSR.
        out[0..2].copy_from_slice(&self.fpu.fcw.to_le_bytes());

        let fsw = self.fpu.fsw_with_top();
        out[2..4].copy_from_slice(&fsw.to_le_bytes());

        out[4] = self.fpu.ftw;
        // out[5] reserved.
        out[6..8].copy_from_slice(&self.fpu.fop.to_le_bytes());

        out[8..12].copy_from_slice(&(self.fpu.fip as u32).to_le_bytes());
        out[12..14].copy_from_slice(&self.fpu.fcs.to_le_bytes());
        // out[14..16] reserved.

        out[16..20].copy_from_slice(&(self.fpu.fdp as u32).to_le_bytes());
        out[20..22].copy_from_slice(&self.fpu.fds.to_le_bytes());
        // out[22..24] reserved.

        out[24..28].copy_from_slice(&self.sse.mxcsr.to_le_bytes());
        out[28..32].copy_from_slice(&MXCSR_MASK.to_le_bytes());

        // 0x20..0xA0: ST/MM register image.
        for (i, reg) in self.fpu.st.iter().enumerate() {
            let start = 32 + i * 16;
            out[start..start + 16].copy_from_slice(&canonicalize_st(*reg).to_le_bytes());
        }

        // 0xA0..0x120: XMM0-7 register image.
        for i in 0..8 {
            let start = 160 + i * 16;
            out[start..start + 16].copy_from_slice(&self.sse.xmm[i].to_le_bytes());
        }

        *dst = out;
    }

    /// `FXSAVE` convenience wrapper that writes the 512-byte image into memory via a [`Bus`].
    pub fn fxsave_to_bus<B: Bus>(&self, bus: &mut B, addr: u64) {
        let mut image = [0u8; FXSAVE_AREA_SIZE];
        self.fxsave(&mut image);
        for (i, byte) in image.iter().copied().enumerate() {
            bus.write_u8(addr + i as u64, byte);
        }
    }

    /// Implements the legacy (32-bit) `FXRSTOR m512byte` memory image.
    pub fn fxrstor(&mut self, src: &[u8; FXSAVE_AREA_SIZE]) -> Result<(), FxStateError> {
        // Intel SDM: if MXCSR is invalid (reserved bits set), `FXRSTOR` raises
        // `#GP(0)` and *does not* restore any state. We model that by validating
        // MXCSR before committing changes to `self`.
        let mxcsr = read_u32(src, 24);
        let mut sse = self.sse.clone();
        // `MXCSR_MASK` is a CPU capability and is ignored by `FXRSTOR` on real
        // hardware, but the *value* must still be validated.
        sse.set_mxcsr(mxcsr)?;

        let fsw = read_u16(src, 2);
        let mut fpu = self.fpu.clone();
        fpu.fcw = read_u16(src, 0);
        fpu.fsw = fsw;
        fpu.top = ((fsw >> 11) & 0b111) as u8;
        fpu.ftw = src[4];
        fpu.fop = read_u16(src, 6);
        fpu.fip = read_u32(src, 8) as u64;
        fpu.fcs = read_u16(src, 12);
        fpu.fdp = read_u32(src, 16) as u64;
        fpu.fds = read_u16(src, 20);

        for i in 0..8 {
            let start = 32 + i * 16;
            fpu.st[i] = canonicalize_st(read_u128(src, start));
        }

        for i in 0..8 {
            let start = 160 + i * 16;
            sse.xmm[i] = read_u128(src, start);
        }

        self.fpu = fpu;
        self.sse = sse;
        Ok(())
    }

    /// `FXRSTOR` convenience wrapper that reads the 512-byte image from memory via a [`Bus`].
    pub fn fxrstor_from_bus<B: Bus>(&mut self, bus: &mut B, addr: u64) -> Result<(), FxStateError> {
        let mut image = [0u8; FXSAVE_AREA_SIZE];
        for i in 0..FXSAVE_AREA_SIZE {
            image[i] = bus.read_u8(addr + i as u64);
        }
        self.fxrstor(&image)
    }

    /// Implements the 64-bit `FXSAVE64 m512byte` memory image.
    pub fn fxsave64(&self, dst: &mut [u8; FXSAVE_AREA_SIZE]) {
        let mut out = [0u8; FXSAVE_AREA_SIZE];

        out[0..2].copy_from_slice(&self.fpu.fcw.to_le_bytes());

        let fsw = self.fpu.fsw_with_top();
        out[2..4].copy_from_slice(&fsw.to_le_bytes());

        out[4] = self.fpu.ftw;
        out[6..8].copy_from_slice(&self.fpu.fop.to_le_bytes());

        out[8..16].copy_from_slice(&self.fpu.fip.to_le_bytes()); // RIP
        out[16..24].copy_from_slice(&self.fpu.fdp.to_le_bytes()); // RDP

        out[24..28].copy_from_slice(&self.sse.mxcsr.to_le_bytes());
        out[28..32].copy_from_slice(&MXCSR_MASK.to_le_bytes());

        for (i, reg) in self.fpu.st.iter().enumerate() {
            let start = 32 + i * 16;
            out[start..start + 16].copy_from_slice(&canonicalize_st(*reg).to_le_bytes());
        }

        // 16 XMM registers in 64-bit mode.
        for i in 0..16 {
            let start = 160 + i * 16;
            out[start..start + 16].copy_from_slice(&self.sse.xmm[i].to_le_bytes());
        }

        *dst = out;
    }

    /// `FXSAVE64` convenience wrapper that writes the 512-byte image into memory via a [`Bus`].
    pub fn fxsave64_to_bus<B: Bus>(&self, bus: &mut B, addr: u64) {
        let mut image = [0u8; FXSAVE_AREA_SIZE];
        self.fxsave64(&mut image);
        for (i, byte) in image.iter().copied().enumerate() {
            bus.write_u8(addr + i as u64, byte);
        }
    }

    /// Implements the 64-bit `FXRSTOR64 m512byte` memory image.
    pub fn fxrstor64(&mut self, src: &[u8; FXSAVE_AREA_SIZE]) -> Result<(), FxStateError> {
        let mxcsr = read_u32(src, 24);
        let mut sse = self.sse.clone();
        sse.set_mxcsr(mxcsr)?;

        let fsw = read_u16(src, 2);
        let mut fpu = self.fpu.clone();
        fpu.fcw = read_u16(src, 0);
        fpu.fsw = fsw;
        fpu.top = ((fsw >> 11) & 0b111) as u8;
        fpu.ftw = src[4];
        fpu.fop = read_u16(src, 6);
        fpu.fip = read_u64(src, 8);
        fpu.fdp = read_u64(src, 16);

        for i in 0..8 {
            let start = 32 + i * 16;
            fpu.st[i] = canonicalize_st(read_u128(src, start));
        }

        for i in 0..16 {
            let start = 160 + i * 16;
            sse.xmm[i] = read_u128(src, start);
        }

        self.fpu = fpu;
        self.sse = sse;
        Ok(())
    }

    /// `FXRSTOR64` convenience wrapper that reads the 512-byte image from memory via a [`Bus`].
    pub fn fxrstor64_from_bus<B: Bus>(
        &mut self,
        bus: &mut B,
        addr: u64,
    ) -> Result<(), FxStateError> {
        let mut image = [0u8; FXSAVE_AREA_SIZE];
        for i in 0..FXSAVE_AREA_SIZE {
            image[i] = bus.read_u8(addr + i as u64);
        }
        self.fxrstor64(&image)
    }
}

fn read_u16(src: &[u8; FXSAVE_AREA_SIZE], offset: usize) -> u16 {
    u16::from_le_bytes(src[offset..offset + 2].try_into().unwrap())
}

fn read_u32(src: &[u8; FXSAVE_AREA_SIZE], offset: usize) -> u32 {
    u32::from_le_bytes(src[offset..offset + 4].try_into().unwrap())
}

fn read_u64(src: &[u8; FXSAVE_AREA_SIZE], offset: usize) -> u64 {
    u64::from_le_bytes(src[offset..offset + 8].try_into().unwrap())
}

fn read_u128(src: &[u8; FXSAVE_AREA_SIZE], offset: usize) -> u128 {
    u128::from_le_bytes(src[offset..offset + 16].try_into().unwrap())
}
