use crate::segmentation::Seg;
use crate::state::{
    CpuMode, CpuState, CR0_PE, CR0_PG, CR4_PAE, EFER_LMA, EFER_LME, SEG_ACCESS_DB, SEG_ACCESS_L,
};

/// Instruction decoding/execution width derived from CS and [`CpuMode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeMode {
    Real16,
    Protected16,
    Protected32,
    Long64,
}

impl CpuState {
    /// Returns the current high-level execution mode classification.
    ///
    /// Prefer reading [`CpuState::mode`] directly unless you need a legacy name.
    #[inline]
    pub fn cpu_mode(&self) -> CpuMode {
        self.mode
    }

    /// Returns the current effective instruction decoding/execution width.
    pub fn code_mode(&self) -> CodeMode {
        match self.mode {
            CpuMode::Real | CpuMode::Vm86 => CodeMode::Real16,
            CpuMode::Protected => {
                if self.segments.cs.is_default_32bit() {
                    CodeMode::Protected32
                } else {
                    CodeMode::Protected16
                }
            }
            CpuMode::Long => CodeMode::Long64,
        }
    }

    pub fn set_cr0(&mut self, value: u64) {
        self.control.cr0 = value;
        self.recompute_lma();
        self.update_mode();
    }

    pub fn set_cr4(&mut self, value: u64) {
        self.control.cr4 = value;
        self.recompute_lma();
        self.update_mode();
    }

    pub fn set_efer(&mut self, value: u64) {
        // EFER.LMA is read-only on real hardware; preserve it across writes.
        let lma = self.msr.efer & EFER_LMA;
        self.msr.efer = (value & !EFER_LMA) | lma;
        self.recompute_lma();
        self.update_mode();
    }

    /// Returns whether long-mode enabling conditions are met (CR0.PG, CR4.PAE, EFER.LME).
    pub fn long_mode_conditions_met(&self) -> bool {
        (self.control.cr0 & CR0_PG != 0) && (self.control.cr4 & CR4_PAE != 0) && (self.msr.efer & EFER_LME != 0)
    }

    /// Model-Specific Register (MSR) backed base for FS/GS in IA-32e mode.
    pub fn msr_seg_base(&self, seg: Seg) -> u64 {
        match seg {
            Seg::FS => self.msr.fs_base,
            Seg::GS => self.msr.gs_base,
            _ => 0,
        }
    }

    /// Implements the SWAPGS instruction semantics (swap GS_BASE and KERNEL_GS_BASE).
    pub fn swapgs(&mut self) {
        core::mem::swap(&mut self.msr.gs_base, &mut self.msr.kernel_gs_base);
    }

    /// Convenience: updates CR0.PE while keeping the rest of CR0 unchanged.
    pub fn set_protected_enable(&mut self, enabled: bool) {
        if enabled {
            self.control.cr0 |= CR0_PE;
        } else {
            self.control.cr0 &= !CR0_PE;
            // Leaving protected mode implicitly leaves IA-32e mode.
            self.msr.efer &= !EFER_LMA;
            // Reset CS.L so `update_mode()` doesn't accidentally classify a future
            // PE transition as 64-bit without a far transfer.
            self.segments.cs.access &= !(SEG_ACCESS_L | SEG_ACCESS_DB);
        }
        self.update_mode();
    }

    /// Recomputes whether IA-32e mode active state (EFER.LMA) is still valid.
    ///
    /// Real hardware clears EFER.LMA when the enabling conditions are no longer met.
    fn recompute_lma(&mut self) {
        if !self.long_mode_conditions_met() {
            self.msr.efer &= !EFER_LMA;
        }
    }
}

