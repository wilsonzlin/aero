use crate::segmentation::Seg;
use crate::CpuState;

pub const CR0_PE: u64 = 1 << 0;
pub const CR0_PG: u64 = 1 << 31;

pub const CR4_PAE: u64 = 1 << 5;

pub const EFER_LME: u64 = 1 << 8;

/// High-level CPU execution mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuMode {
    Real,
    Protected,
    Long,
}

/// Instruction decoding/execution width derived from CS and mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeMode {
    Real16,
    Protected16,
    Protected32,
    Long64,
}

impl CpuState {
    pub fn cpu_mode(&self) -> CpuMode {
        if self.lma {
            CpuMode::Long
        } else if self.cr0 & CR0_PE != 0 {
            CpuMode::Protected
        } else {
            CpuMode::Real
        }
    }

    pub fn code_mode(&self) -> CodeMode {
        match self.cpu_mode() {
            CpuMode::Real => CodeMode::Real16,
            CpuMode::Protected => {
                if self.segments.cs.cache.attrs.default_big {
                    CodeMode::Protected32
                } else {
                    CodeMode::Protected16
                }
            }
            CpuMode::Long => CodeMode::Long64,
        }
    }

    pub fn set_cr0(&mut self, value: u64) {
        self.cr0 = value;
        self.recompute_lma();
    }

    pub fn set_cr4(&mut self, value: u64) {
        self.cr4 = value;
        self.recompute_lma();
    }

    pub fn set_efer(&mut self, value: u64) {
        self.efer = value;
        self.recompute_lma();
    }

    /// Recomputes whether long-mode active state is still valid.
    ///
    /// Per this project's simplified model, long mode only becomes active after a far
    /// transfer loads a 64-bit code segment. However, long mode must be cleared when
    /// the enabling conditions are no longer met (paging disabled, etc.).
    fn recompute_lma(&mut self) {
        let enabled =
            (self.cr0 & CR0_PG != 0) && (self.cr4 & CR4_PAE != 0) && (self.efer & EFER_LME != 0);
        if !enabled {
            self.lma = false;
        }
    }

    /// Returns whether long-mode activation conditions are met (CR0.PG, CR4.PAE, EFER.LME).
    pub fn long_mode_conditions_met(&self) -> bool {
        (self.cr0 & CR0_PG != 0) && (self.cr4 & CR4_PAE != 0) && (self.efer & EFER_LME != 0)
    }

    /// Model-Specific Register (MSR) backed base for FS/GS in long mode.
    pub fn msr_seg_base(&self, seg: Seg) -> u64 {
        match seg {
            Seg::FS => self.msr_fs_base,
            Seg::GS => self.msr_gs_base,
            _ => 0,
        }
    }

    /// Implements the SWAPGS instruction semantics (swap GS_BASE and KERNEL_GS_BASE).
    pub fn swapgs(&mut self) {
        core::mem::swap(&mut self.msr_gs_base, &mut self.msr_kernel_gs_base);
    }
}
