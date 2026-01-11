//! Model-specific register (MSR) state.
//!
//! Windows heavily relies on MSRs for fast system calls, GS base swapping, and
//! APIC programming. We model the subset required for Windows 7 boot/runtime.

use crate::cpuid::{bits as cpuid_bits, CpuFeatures};
use crate::state;
use crate::Exception;

// Common MSR indices used by Windows 7.
pub const IA32_TSC: u32 = 0x0000_0010;
pub const IA32_APIC_BASE: u32 = 0x0000_001B;

pub const IA32_SYSENTER_CS: u32 = 0x0000_0174;
pub const IA32_SYSENTER_ESP: u32 = 0x0000_0175;
pub const IA32_SYSENTER_EIP: u32 = 0x0000_0176;

pub const IA32_EFER: u32 = 0xC000_0080;
pub const IA32_STAR: u32 = 0xC000_0081;
pub const IA32_LSTAR: u32 = 0xC000_0082;
pub const IA32_CSTAR: u32 = 0xC000_0083;
pub const IA32_FMASK: u32 = 0xC000_0084;

pub const IA32_FS_BASE: u32 = 0xC000_0100;
pub const IA32_GS_BASE: u32 = 0xC000_0101;
pub const IA32_KERNEL_GS_BASE: u32 = 0xC000_0102;
pub const IA32_TSC_AUX: u32 = 0xC000_0103;

// IA32_EFER bits (subset).
pub const EFER_SCE: u64 = 1 << 0;
pub const EFER_LME: u64 = 1 << 8;
pub const EFER_LMA: u64 = 1 << 10;
pub const EFER_NXE: u64 = 1 << 11;

/// Canonical MSR storage used by [`crate::state::CpuState`].
///
/// We keep the backing fields inside `state::CpuState` so the interpreter/JIT ABI
/// remains stable, but expose MSR indices and helpers from this module.
pub type MsrState = state::MsrState;

impl state::MsrState {
    /// Read an MSR value.
    ///
    /// Unknown MSRs raise `#GP(0)` instead of being silently ignored.
    pub fn read(&self, msr: u32) -> Result<u64, Exception> {
        match msr {
            IA32_EFER => Ok(self.efer),
            IA32_STAR => Ok(self.star),
            IA32_LSTAR => Ok(self.lstar),
            IA32_CSTAR => Ok(self.cstar),
            IA32_FMASK => Ok(self.fmask),
            IA32_SYSENTER_CS => Ok(self.sysenter_cs),
            IA32_SYSENTER_ESP => Ok(self.sysenter_esp),
            IA32_SYSENTER_EIP => Ok(self.sysenter_eip),
            IA32_FS_BASE => Ok(self.fs_base),
            IA32_GS_BASE => Ok(self.gs_base),
            IA32_KERNEL_GS_BASE => Ok(self.kernel_gs_base),
            IA32_APIC_BASE => Ok(self.apic_base),
            IA32_TSC_AUX => Ok(u64::from(self.tsc_aux)),
            _ => Err(Exception::gp0()),
        }
    }

    /// Write an MSR value.
    ///
    /// Unknown MSRs raise `#GP(0)`.
    pub fn write(&mut self, features: &CpuFeatures, msr: u32, value: u64) -> Result<(), Exception> {
        match msr {
            IA32_EFER => {
                // Keep CPUID/MSR coherent: if a feature is not advertised, mask its controlling
                // EFER bit rather than letting the guest enable it.
                let mut next = value;
                // LMA is read-only (controlled by paging mode); preserve the stored bit.
                next = (next & !EFER_LMA) | (self.efer & EFER_LMA);

                if (features.ext1_edx & cpuid_bits::EXT1_EDX_SYSCALL) == 0 {
                    next &= !EFER_SCE;
                }
                if (features.ext1_edx & cpuid_bits::EXT1_EDX_LM) == 0 {
                    next &= !EFER_LME;
                }
                if (features.ext1_edx & cpuid_bits::EXT1_EDX_NX) == 0 {
                    next &= !EFER_NXE;
                }

                self.efer = next;
                Ok(())
            }
            IA32_STAR => {
                self.star = value;
                Ok(())
            }
            IA32_LSTAR => {
                self.lstar = value;
                Ok(())
            }
            IA32_CSTAR => {
                self.cstar = value;
                Ok(())
            }
            IA32_FMASK => {
                self.fmask = value;
                Ok(())
            }
            IA32_SYSENTER_CS => {
                self.sysenter_cs = value;
                Ok(())
            }
            IA32_SYSENTER_ESP => {
                self.sysenter_esp = value;
                Ok(())
            }
            IA32_SYSENTER_EIP => {
                self.sysenter_eip = value;
                Ok(())
            }
            IA32_FS_BASE => {
                self.fs_base = value;
                Ok(())
            }
            IA32_GS_BASE => {
                self.gs_base = value;
                Ok(())
            }
            IA32_KERNEL_GS_BASE => {
                self.kernel_gs_base = value;
                Ok(())
            }
            IA32_APIC_BASE => {
                self.apic_base = value;
                Ok(())
            }
            IA32_TSC_AUX => {
                self.tsc_aux = (value & 0xFFFF_FFFF) as u32;
                Ok(())
            }
            _ => Err(Exception::gp0()),
        }
    }
}
