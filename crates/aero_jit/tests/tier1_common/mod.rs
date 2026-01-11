#![allow(dead_code)]

use aero_cpu_core::state::{
    CpuState, RFLAGS_AF, RFLAGS_CF, RFLAGS_OF, RFLAGS_PF, RFLAGS_SF, RFLAGS_ZF,
};
use aero_jit::{abi, Tier1Bus};
use aero_types::{Flag, Gpr, Width};

#[derive(Clone, Debug)]
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

impl Tier1Bus for SimpleBus {
    fn read_u8(&self, addr: u64) -> u8 {
        self.mem[addr as usize]
    }

    fn write_u8(&mut self, addr: u64, value: u8) {
        self.mem[addr as usize] = value;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuSnapshot {
    pub gpr: [u64; 16],
    pub rip: u64,
    pub rflags: u64,
}

impl CpuSnapshot {
    #[must_use]
    pub fn from_cpu(cpu: &CpuState) -> Self {
        Self {
            gpr: cpu.gpr,
            rip: cpu.rip,
            rflags: cpu.rflags_snapshot(),
        }
    }

    #[must_use]
    pub fn from_wasm_bytes(bytes: &[u8]) -> Self {
        let mut gpr = [0u64; 16];
        for (i, slot) in gpr.iter_mut().enumerate() {
            let off = abi::CPU_GPR_OFF[i] as usize;
            let mut buf = [0u8; 8];
            buf.copy_from_slice(&bytes[off..off + 8]);
            *slot = u64::from_le_bytes(buf);
        }

        let mut buf = [0u8; 8];
        let rip_off = abi::CPU_RIP_OFF as usize;
        buf.copy_from_slice(&bytes[rip_off..rip_off + 8]);
        let rip = u64::from_le_bytes(buf);

        let rflags_off = abi::CPU_RFLAGS_OFF as usize;
        buf.copy_from_slice(&bytes[rflags_off..rflags_off + 8]);
        let rflags = u64::from_le_bytes(buf);

        Self { gpr, rip, rflags }
    }
}

#[inline]
pub fn read_gpr(cpu: &CpuState, reg: Gpr) -> u64 {
    cpu.gpr[reg.as_u8() as usize]
}

#[inline]
pub fn write_gpr(cpu: &mut CpuState, reg: Gpr, value: u64) {
    cpu.gpr[reg.as_u8() as usize] = value;
}

#[inline]
pub fn read_gpr_part(cpu: &CpuState, reg: Gpr, width: Width, high8: bool) -> u64 {
    let val = read_gpr(cpu, reg);
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

#[inline]
pub fn write_gpr_part(cpu: &mut CpuState, reg: Gpr, width: Width, high8: bool, value: u64) {
    let idx = reg.as_u8() as usize;
    let prev = cpu.gpr[idx];
    let masked = width.truncate(value);
    cpu.gpr[idx] = match width {
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

#[inline]
fn flag_mask(flag: Flag) -> u64 {
    match flag {
        Flag::Cf => RFLAGS_CF,
        Flag::Pf => RFLAGS_PF,
        Flag::Af => RFLAGS_AF,
        Flag::Zf => RFLAGS_ZF,
        Flag::Sf => RFLAGS_SF,
        Flag::Of => RFLAGS_OF,
    }
}

#[inline]
pub fn read_flag(cpu: &CpuState, flag: Flag) -> bool {
    cpu.get_flag(flag_mask(flag))
}

#[inline]
pub fn write_flag(cpu: &mut CpuState, flag: Flag, value: bool) {
    cpu.set_flag(flag_mask(flag), value);
}

pub fn write_cpu_to_wasm_bytes(cpu: &CpuState, bytes: &mut [u8]) {
    assert_eq!(
        bytes.len(),
        abi::CPU_STATE_SIZE as usize,
        "unexpected cpu state buffer size"
    );

    for i in 0..16 {
        let off = abi::CPU_GPR_OFF[i] as usize;
        bytes[off..off + 8].copy_from_slice(&cpu.gpr[i].to_le_bytes());
    }

    let rip_off = abi::CPU_RIP_OFF as usize;
    bytes[rip_off..rip_off + 8].copy_from_slice(&cpu.rip.to_le_bytes());

    let rflags = cpu.rflags_snapshot();
    let rflags_off = abi::CPU_RFLAGS_OFF as usize;
    bytes[rflags_off..rflags_off + 8].copy_from_slice(&rflags.to_le_bytes());
}
