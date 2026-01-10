#![forbid(unsafe_code)]

use core::fmt;

pub const IA32_TSC: u32 = 0x0000_0010;
pub const IA32_APIC_BASE: u32 = 0x0000_001B;
pub const IA32_SYSENTER_CS: u32 = 0x0000_0174;
pub const IA32_SYSENTER_ESP: u32 = 0x0000_0175;
pub const IA32_SYSENTER_EIP: u32 = 0x0000_0176;
pub const IA32_PAT: u32 = 0x0000_0277;

pub const IA32_EFER: u32 = 0xC000_0080;
pub const IA32_STAR: u32 = 0xC000_0081;
pub const IA32_LSTAR: u32 = 0xC000_0082;
pub const IA32_CSTAR: u32 = 0xC000_0083;
pub const IA32_SFMASK: u32 = 0xC000_0084;

pub const IA32_FS_BASE: u32 = 0xC000_0100;
pub const IA32_GS_BASE: u32 = 0xC000_0101;
pub const IA32_KERNEL_GS_BASE: u32 = 0xC000_0102;
pub const IA32_TSC_AUX: u32 = 0xC000_0103;

pub const EFER_SCE: u64 = 1 << 0;
pub const EFER_LME: u64 = 1 << 8;
pub const EFER_LMA: u64 = 1 << 10; // read-only; derived from CR0.PG + CR4.PAE + EFER.LME
pub const EFER_NXE: u64 = 1 << 11;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MsrError {
    Unknown { msr: u32 },
}

impl fmt::Display for MsrError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MsrError::Unknown { msr } => write!(f, "unknown MSR {msr:#x}"),
        }
    }
}

impl std::error::Error for MsrError {}

#[derive(Debug, Clone)]
pub struct Msrs {
    pub efer: u64,
    pub star: u64,
    pub lstar: u64,
    pub cstar: u64,
    pub sfmask: u64,
    pub sysenter_cs: u64,
    pub sysenter_esp: u64,
    pub sysenter_eip: u64,
    pub fs_base: u64,
    pub gs_base: u64,
    pub kernel_gs_base: u64,
    pub tsc: u64,
    pub apic_base: u64,
    pub pat: u64,
    pub tsc_aux: u32,
}

impl Default for Msrs {
    fn default() -> Self {
        Self {
            efer: 0,
            star: 0,
            lstar: 0,
            cstar: 0,
            sfmask: 0,
            sysenter_cs: 0,
            sysenter_esp: 0,
            sysenter_eip: 0,
            fs_base: 0,
            gs_base: 0,
            kernel_gs_base: 0,
            tsc: 0,
            // Typical reset value: APIC enabled at 0xFEE00000, BSP bit set.
            apic_base: 0xFEE0_0000 | (1 << 11) | (1 << 8),
            // Intel SDM: the default PAT MSR reset value encodes WB/WT/UC-/UC.
            pat: 0x0007_0406_0007_0406,
            tsc_aux: 0,
        }
    }
}

impl Msrs {
    pub fn read(&self, msr: u32) -> Result<u64, MsrError> {
        match msr {
            IA32_EFER => Ok(self.efer),
            IA32_STAR => Ok(self.star),
            IA32_LSTAR => Ok(self.lstar),
            IA32_CSTAR => Ok(self.cstar),
            IA32_SFMASK => Ok(self.sfmask),
            IA32_SYSENTER_CS => Ok(self.sysenter_cs),
            IA32_SYSENTER_ESP => Ok(self.sysenter_esp),
            IA32_SYSENTER_EIP => Ok(self.sysenter_eip),
            IA32_FS_BASE => Ok(self.fs_base),
            IA32_GS_BASE => Ok(self.gs_base),
            IA32_KERNEL_GS_BASE => Ok(self.kernel_gs_base),
            IA32_TSC => Ok(self.tsc),
            IA32_APIC_BASE => Ok(self.apic_base),
            IA32_PAT => Ok(self.pat),
            IA32_TSC_AUX => Ok(self.tsc_aux as u64),
            _ => Err(MsrError::Unknown { msr }),
        }
    }

    pub fn write(&mut self, msr: u32, val: u64) -> Result<(), MsrError> {
        match msr {
            IA32_EFER => {
                // LMA is derived; ignore writes to it.
                let lma = self.efer & EFER_LMA;
                self.efer = (val & !EFER_LMA) | lma;
                Ok(())
            }
            IA32_STAR => {
                self.star = val;
                Ok(())
            }
            IA32_LSTAR => {
                self.lstar = val;
                Ok(())
            }
            IA32_CSTAR => {
                self.cstar = val;
                Ok(())
            }
            IA32_SFMASK => {
                self.sfmask = val;
                Ok(())
            }
            IA32_SYSENTER_CS => {
                self.sysenter_cs = val;
                Ok(())
            }
            IA32_SYSENTER_ESP => {
                self.sysenter_esp = val;
                Ok(())
            }
            IA32_SYSENTER_EIP => {
                self.sysenter_eip = val;
                Ok(())
            }
            IA32_FS_BASE => {
                self.fs_base = val;
                Ok(())
            }
            IA32_GS_BASE => {
                self.gs_base = val;
                Ok(())
            }
            IA32_KERNEL_GS_BASE => {
                self.kernel_gs_base = val;
                Ok(())
            }
            IA32_TSC => {
                self.tsc = val;
                Ok(())
            }
            IA32_APIC_BASE => {
                self.apic_base = val;
                Ok(())
            }
            IA32_PAT => {
                self.pat = val;
                Ok(())
            }
            IA32_TSC_AUX => {
                self.tsc_aux = val as u32;
                Ok(())
            }
            _ => Err(MsrError::Unknown { msr }),
        }
    }
}
