//! Helpers for bringing vCPUs into architecturally plausible x86 reset / SIPI states.
//!
//! These helpers are used by SMP bring-up code (INIT + SIPI) to reset an AP back into a clean
//! real-mode baseline and then enter at a SIPI vector address.

#![allow(dead_code)]

use aero_cpu_core::state::{CpuMode, CpuState, Segment, RFLAGS_RESERVED1};
use aero_cpu_core::CpuCore;

// VMX-style "segment access rights" encoding used throughout the emulator.
const REAL_MODE_CODE_ACCESS: u32 = 0x9B; // present, DPL0, code, readable, accessed
const REAL_MODE_DATA_ACCESS: u32 = 0x93; // present, DPL0, data, writable, accessed

fn set_real_mode_segment(seg: &mut Segment, selector: u16, access: u32) {
    seg.selector = selector;
    seg.base = (selector as u64) << 4;
    seg.limit = 0xFFFF;
    seg.access = access;
}

fn init_real_mode_segment_registers(state: &mut CpuState) {
    set_real_mode_segment(&mut state.segments.cs, 0, REAL_MODE_CODE_ACCESS);
    set_real_mode_segment(&mut state.segments.ds, 0, REAL_MODE_DATA_ACCESS);
    set_real_mode_segment(&mut state.segments.es, 0, REAL_MODE_DATA_ACCESS);
    set_real_mode_segment(&mut state.segments.ss, 0, REAL_MODE_DATA_ACCESS);
    set_real_mode_segment(&mut state.segments.fs, 0, REAL_MODE_DATA_ACCESS);
    set_real_mode_segment(&mut state.segments.gs, 0, REAL_MODE_DATA_ACCESS);
}

/// Reset a vCPU core state to a clean real-mode baseline appropriate for an AP after INIT.
///
/// This intentionally resets most architectural state but preserves:
/// - the local APIC base MSR (so the BSP bit does not regress),
/// - TSC/TSC_AUX and the [`CpuCore::time`] tracking.
pub(crate) fn reset_ap_vcpu_to_init_state(cpu: &mut CpuCore) {
    let preserved_tsc = cpu.state.msr.tsc;
    let preserved_tsc_aux = cpu.state.msr.tsc_aux;
    // The BSP bit in IA32_APIC_BASE is fixed per-CPU on real hardware (set only on the BSP).
    // When initializing an *AP* we always clear it, even if the caller forgot to.
    let preserved_apic_base = cpu.state.msr.apic_base & !(1 << 8);
    let preserved_a20 = cpu.state.a20_enabled;

    let mut state = CpuState::new(CpuMode::Real);
    state.msr.tsc = preserved_tsc;
    state.msr.tsc_aux = preserved_tsc_aux;
    state.msr.apic_base = preserved_apic_base;
    state.a20_enabled = preserved_a20;

    init_real_mode_segment_registers(&mut state);

    // Real-mode INIT baseline starts with IP=0 and flags in a deterministic state.
    state.set_ip(0);
    state.set_rflags(RFLAGS_RESERVED1);

    // INIT clears HLT state and any BIOS interrupt stub markers.
    state.halted = false;
    state.clear_pending_bios_int();

    cpu.state = state;
    cpu.pending = Default::default();
    // Keep the time source coherent with the (preserved) architectural TSC value.
    cpu.time.set_tsc(cpu.state.msr.tsc);
}

/// Initialise a vCPU core to begin executing from a SIPI vector in real mode.
///
/// The SIPI vector `v` encodes a 4KiB page number: the AP begins at physical address `v<<12`
/// with `CS:IP = v<<8:0`.
pub(crate) fn init_ap_vcpu_from_sipi(cpu: &mut CpuCore, vector: u8) {
    reset_ap_vcpu_to_init_state(cpu);

    let cs_selector = (vector as u16) << 8;
    set_real_mode_segment(
        &mut cpu.state.segments.cs,
        cs_selector,
        REAL_MODE_CODE_ACCESS,
    );

    // IP is 0 (linear execution begins at CS.base).
    cpu.state.set_ip(0);

    // SIPI always begins in real mode with the reserved RFLAGS bit set.
    cpu.state.set_rflags(RFLAGS_RESERVED1);

    // Ensure the CPU is runnable.
    cpu.state.halted = false;
    cpu.state.clear_pending_bios_int();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smp_init_sipi_starts_ap() {
        let mut cpu = CpuCore::new(CpuMode::Real);
        cpu.state.halted = true;
        cpu.state.set_pending_bios_int(0x13);

        // Simulate a non-BSP APIC base MSR so the helper preserves the BSP bit setting.
        cpu.state.msr.apic_base = 0xFEE0_0000 | (1 << 11);

        init_ap_vcpu_from_sipi(&mut cpu, 0x08);

        assert_eq!(cpu.state.segments.cs.base, 0x8000);
        assert_eq!(cpu.state.segments.cs.selector, 0x0800);

        for seg in [
            &cpu.state.segments.ds,
            &cpu.state.segments.es,
            &cpu.state.segments.ss,
        ] {
            assert_eq!(seg.base, 0);
            assert_eq!(seg.limit, 0xFFFF);
        }

        assert_ne!(cpu.state.rflags() & RFLAGS_RESERVED1, 0);
        assert!(!cpu.state.halted);
        assert_eq!(cpu.state.take_pending_bios_int(), None);
    }

    #[test]
    fn smp_init_resets_ap_to_clean_real_mode_baseline() {
        let mut cpu = CpuCore::new(CpuMode::Long);

        // Dirty the state so we can verify the reset cleans it up.
        cpu.state.mode = CpuMode::Long;
        cpu.state.segments.cs.selector = 0x33;
        cpu.state.segments.cs.base = 0x1234_5678;
        cpu.state.segments.cs.limit = 0;
        cpu.state.segments.cs.access = 0xFFFF_FFFF;
        cpu.state.halted = true;
        cpu.state.set_pending_bios_int(0x10);
        cpu.pending.inject_external_interrupt(0x40);

        cpu.state.msr.tsc = 0x1122_3344_5566_7788;
        cpu.state.msr.apic_base = 0xFEE0_0000 | (1 << 11) | (1 << 8); // BSP bit set (should clear)

        reset_ap_vcpu_to_init_state(&mut cpu);

        assert_eq!(cpu.state.mode, CpuMode::Real);
        assert_eq!(cpu.state.get_ip(), 0);
        assert_ne!(cpu.state.rflags() & RFLAGS_RESERVED1, 0);
        assert!(!cpu.state.halted);
        assert_eq!(cpu.state.take_pending_bios_int(), None);
        assert!(cpu.pending.external_interrupts().is_empty());

        assert_eq!(cpu.state.segments.cs.selector, 0);
        assert_eq!(cpu.state.segments.cs.base, 0);
        assert_eq!(cpu.state.segments.cs.limit, 0xFFFF);
        assert_eq!(cpu.state.segments.cs.access, REAL_MODE_CODE_ACCESS);

        for seg in [
            &cpu.state.segments.ds,
            &cpu.state.segments.es,
            &cpu.state.segments.ss,
            &cpu.state.segments.fs,
            &cpu.state.segments.gs,
        ] {
            assert_eq!(seg.selector, 0);
            assert_eq!(seg.base, 0);
            assert_eq!(seg.limit, 0xFFFF);
            assert_eq!(seg.access, REAL_MODE_DATA_ACCESS);
        }

        // BSP bit must be clear for an AP.
        assert_eq!(cpu.state.msr.apic_base & (1 << 8), 0);
        assert_eq!(cpu.state.msr.tsc, 0x1122_3344_5566_7788);
        assert_eq!(cpu.time.read_tsc(), cpu.state.msr.tsc);
    }
}
