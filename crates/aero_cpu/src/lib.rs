//! Minimal CPU state + bus used by `aero_jit` unit tests.
//!
//! This is *not* a full emulator CPU core. It only provides the pieces the
//! Tier-1 JIT front-end needs: architectural registers, a small subset of
//! flags, and a byte-addressable memory bus.

use aero_types::{Flag, Gpr, Width};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpuState {
    pub gpr: [u64; 16],
    pub rip: u64,
    pub rflags: u64,
}

impl Default for CpuState {
    fn default() -> Self {
        Self { gpr: [0; 16], rip: 0, rflags: 0x2 }
    }
}

impl CpuState {
    #[must_use]
    pub fn read_flag(&self, flag: Flag) -> bool {
        ((self.rflags >> flag.rflags_bit()) & 1) != 0
    }

    pub fn write_flag(&mut self, flag: Flag, val: bool) {
        let bit = 1u64 << flag.rflags_bit();
        if val {
            self.rflags |= bit;
        } else {
            self.rflags &= !bit;
        }
    }

    #[must_use]
    pub fn read_gpr(&self, reg: Gpr) -> u64 {
        self.gpr[reg.as_u8() as usize]
    }

    pub fn write_gpr(&mut self, reg: Gpr, value: u64) {
        self.gpr[reg.as_u8() as usize] = value;
    }

    /// Read a sub-register (8/16/32/64) from a full 64-bit GPR.
    ///
    /// If `high8` is set for 8-bit accesses, bits 8..=15 (AH/CH/DH/BH) are read.
    #[must_use]
    pub fn read_gpr_part(&self, reg: Gpr, width: Width, high8: bool) -> u64 {
        let val = self.read_gpr(reg);
        match width {
            Width::W8 => {
                if high8 {
                    debug_assert!(matches!(reg, Gpr::Rax | Gpr::Rcx | Gpr::Rdx | Gpr::Rbx));
                    (val >> 8) & 0xff
                } else {
                    val & 0xff
                }
            }
            Width::W16 => val & 0xffff,
            Width::W32 => val & 0xffff_ffff,
            Width::W64 => val,
        }
    }

    /// Write a sub-register (8/16/32/64) into a full 64-bit GPR.
    ///
    /// x86-64 semantics:
    /// - 8/16-bit writes only update the low bits (or AH..BH for `high8`).
    /// - 32-bit writes zero-extend into 64-bit.
    pub fn write_gpr_part(&mut self, reg: Gpr, width: Width, high8: bool, value: u64) {
        let idx = reg.as_u8() as usize;
        let prev = self.gpr[idx];
        let masked = width.truncate(value);
        self.gpr[idx] = match width {
            Width::W8 => {
                if high8 {
                    debug_assert!(matches!(reg, Gpr::Rax | Gpr::Rcx | Gpr::Rdx | Gpr::Rbx));
                    (prev & !0xff00) | ((masked & 0xff) << 8)
                } else {
                    (prev & !0xff) | (masked & 0xff)
                }
            }
            Width::W16 => (prev & !0xffff) | (masked & 0xffff),
            Width::W32 => masked & 0xffff_ffff,
            Width::W64 => masked,
        };
    }
}

pub trait CpuBus {
    fn read_u8(&self, addr: u64) -> u8;
    fn write_u8(&mut self, addr: u64, value: u8);

    #[must_use]
    fn read(&self, addr: u64, width: Width) -> u64 {
        match width {
            Width::W8 => self.read_u8(addr) as u64,
            Width::W16 => {
                let b0 = self.read_u8(addr) as u64;
                let b1 = self.read_u8(addr + 1) as u64;
                b0 | (b1 << 8)
            }
            Width::W32 => {
                let mut out = 0u64;
                for i in 0..4 {
                    out |= (self.read_u8(addr + i) as u64) << (i * 8);
                }
                out
            }
            Width::W64 => {
                let mut out = 0u64;
                for i in 0..8 {
                    out |= (self.read_u8(addr + i) as u64) << (i * 8);
                }
                out
            }
        }
    }

    fn write(&mut self, addr: u64, width: Width, value: u64) {
        let v = width.truncate(value);
        match width {
            Width::W8 => self.write_u8(addr, v as u8),
            Width::W16 => {
                self.write_u8(addr, v as u8);
                self.write_u8(addr + 1, (v >> 8) as u8);
            }
            Width::W32 => {
                for i in 0..4 {
                    self.write_u8(addr + i, (v >> (i * 8)) as u8);
                }
            }
            Width::W64 => {
                for i in 0..8 {
                    self.write_u8(addr + i, (v >> (i * 8)) as u8);
                }
            }
        }
    }

    #[must_use]
    fn fetch(&self, addr: u64, len: usize) -> Vec<u8> {
        let mut buf = vec![0u8; len];
        for (i, b) in buf.iter_mut().enumerate() {
            *b = self.read_u8(addr + i as u64);
        }
        buf
    }
}

#[derive(Debug, Clone)]
pub struct SimpleBus {
    mem: Vec<u8>,
}

impl SimpleBus {
    #[must_use]
    pub fn new(size: usize) -> Self {
        Self { mem: vec![0; size] }
    }

    pub fn load(&mut self, addr: u64, data: &[u8]) {
        let start = addr as usize;
        let end = start + data.len();
        self.mem[start..end].copy_from_slice(data);
    }

    #[must_use]
    pub fn mem(&self) -> &[u8] {
        &self.mem
    }
}

impl CpuBus for SimpleBus {
    fn read_u8(&self, addr: u64) -> u8 {
        self.mem[addr as usize]
    }

    fn write_u8(&mut self, addr: u64, value: u8) {
        self.mem[addr as usize] = value;
    }
}

