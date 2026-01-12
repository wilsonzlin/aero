use aero_cpu_core::fpu::canonicalize_st;
use aero_cpu_core::sse_state::MXCSR_MASK;
use aero_cpu_core::state::{gpr as core_gpr, CpuMode as CoreCpuMode, CpuState as CoreCpuState};

use crate::types::{CpuInternalState, CpuMode, CpuState, FpuState, MmuState, SegmentState};

pub fn snapshot_from_cpu_core(core: &CoreCpuState) -> (CpuState, MmuState) {
    (cpu_state_from_cpu_core(core), mmu_state_from_cpu_core(core))
}

pub fn cpu_internal_state_from_cpu_core(core: &aero_cpu_core::CpuCore) -> CpuInternalState {
    CpuInternalState {
        interrupt_inhibit: core.pending.interrupt_inhibit(),
        pending_external_interrupts: core.pending.external_interrupts.iter().copied().collect(),
    }
}

pub fn apply_cpu_internal_state_to_cpu_core(
    state: &CpuInternalState,
    core: &mut aero_cpu_core::CpuCore,
) {
    // CPU_INTERNAL v2 only captures the external interrupt FIFO and the interrupt shadow counter.
    // Any other non-architectural pending-event state is intentionally not represented and is
    // cleared here to avoid resuming with stale bookkeeping.
    core.pending = Default::default();

    core.pending
        .set_interrupt_inhibit_for_restore(state.interrupt_inhibit);
    core.pending
        .external_interrupts
        .extend(state.pending_external_interrupts.iter().copied());
}

pub fn cpu_core_from_snapshot(cpu: &CpuState, mmu: &MmuState) -> CoreCpuState {
    let mut core = CoreCpuState::default();
    apply_cpu_state_to_cpu_core(cpu, &mut core);
    apply_mmu_state_to_cpu_core(mmu, &mut core);
    core
}

pub fn cpu_state_from_cpu_core(core: &CoreCpuState) -> CpuState {
    let mut core = core.clone();
    // Snapshot encoding stores materialized RFLAGS (no lazy flags).
    core.commit_lazy_flags();

    CpuState {
        rax: core.gpr[core_gpr::RAX],
        rcx: core.gpr[core_gpr::RCX],
        rdx: core.gpr[core_gpr::RDX],
        rbx: core.gpr[core_gpr::RBX],
        rsp: core.gpr[core_gpr::RSP],
        rbp: core.gpr[core_gpr::RBP],
        rsi: core.gpr[core_gpr::RSI],
        rdi: core.gpr[core_gpr::RDI],
        r8: core.gpr[core_gpr::R8],
        r9: core.gpr[core_gpr::R9],
        r10: core.gpr[core_gpr::R10],
        r11: core.gpr[core_gpr::R11],
        r12: core.gpr[core_gpr::R12],
        r13: core.gpr[core_gpr::R13],
        r14: core.gpr[core_gpr::R14],
        r15: core.gpr[core_gpr::R15],
        rip: core.rip,
        rflags: core.rflags,
        mode: match core.mode {
            CoreCpuMode::Real => CpuMode::Real,
            CoreCpuMode::Protected => CpuMode::Protected,
            CoreCpuMode::Long => CpuMode::Long,
            CoreCpuMode::Vm86 => CpuMode::Vm86,
        },
        halted: core.halted,
        pending_bios_int: core.pending_bios_int,
        pending_bios_int_valid: core.pending_bios_int_valid,
        a20_enabled: core.a20_enabled,
        irq13_pending: core.irq13_pending,
        es: segment_from_core(&core.segments.es),
        cs: segment_from_core(&core.segments.cs),
        ss: segment_from_core(&core.segments.ss),
        ds: segment_from_core(&core.segments.ds),
        fs: segment_from_core(&core.segments.fs),
        gs: segment_from_core(&core.segments.gs),
        fpu: FpuState {
            fcw: core.fpu.fcw,
            fsw: core.fpu.fsw,
            ftw: core.fpu.ftw,
            top: core.fpu.top,
            fop: core.fpu.fop,
            fip: core.fpu.fip,
            fdp: core.fpu.fdp,
            fcs: core.fpu.fcs,
            fds: core.fpu.fds,
            st: core.fpu.st,
        },
        mxcsr: core.sse.mxcsr,
        xmm: core.sse.xmm,
        fxsave: fxsave64_bytes(&core.fpu, &core.sse),
    }
}

pub fn mmu_state_from_cpu_core(core: &CoreCpuState) -> MmuState {
    MmuState {
        cr0: core.control.cr0,
        cr2: core.control.cr2,
        cr3: core.control.cr3,
        cr4: core.control.cr4,
        cr8: core.control.cr8,

        gdtr_base: core.tables.gdtr.base,
        gdtr_limit: core.tables.gdtr.limit,
        idtr_base: core.tables.idtr.base,
        idtr_limit: core.tables.idtr.limit,
        ldtr: segment_from_core(&core.tables.ldtr),
        tr: segment_from_core(&core.tables.tr),

        dr0: core.debug.dr[0],
        dr1: core.debug.dr[1],
        dr2: core.debug.dr[2],
        dr3: core.debug.dr[3],
        dr6: core.debug.dr6,
        dr7: core.debug.dr7,

        efer: core.msr.efer,
        star: core.msr.star,
        lstar: core.msr.lstar,
        cstar: core.msr.cstar,
        sfmask: core.msr.fmask,
        sysenter_cs: core.msr.sysenter_cs,
        sysenter_eip: core.msr.sysenter_eip,
        sysenter_esp: core.msr.sysenter_esp,
        fs_base: core.msr.fs_base,
        gs_base: core.msr.gs_base,
        kernel_gs_base: core.msr.kernel_gs_base,
        apic_base: core.msr.apic_base,
        tsc: core.msr.tsc,
        ..Default::default()
    }
}

pub fn apply_cpu_state_to_cpu_core(cpu: &CpuState, core: &mut CoreCpuState) {
    core.gpr[core_gpr::RAX] = cpu.rax;
    core.gpr[core_gpr::RCX] = cpu.rcx;
    core.gpr[core_gpr::RDX] = cpu.rdx;
    core.gpr[core_gpr::RBX] = cpu.rbx;
    core.gpr[core_gpr::RSP] = cpu.rsp;
    core.gpr[core_gpr::RBP] = cpu.rbp;
    core.gpr[core_gpr::RSI] = cpu.rsi;
    core.gpr[core_gpr::RDI] = cpu.rdi;
    core.gpr[core_gpr::R8] = cpu.r8;
    core.gpr[core_gpr::R9] = cpu.r9;
    core.gpr[core_gpr::R10] = cpu.r10;
    core.gpr[core_gpr::R11] = cpu.r11;
    core.gpr[core_gpr::R12] = cpu.r12;
    core.gpr[core_gpr::R13] = cpu.r13;
    core.gpr[core_gpr::R14] = cpu.r14;
    core.gpr[core_gpr::R15] = cpu.r15;

    core.rip = cpu.rip;
    core.set_rflags(cpu.rflags);

    core.mode = match cpu.mode {
        CpuMode::Real => CoreCpuMode::Real,
        CpuMode::Protected => CoreCpuMode::Protected,
        CpuMode::Long => CoreCpuMode::Long,
        CpuMode::Vm86 => CoreCpuMode::Vm86,
    };
    core.halted = cpu.halted;
    core.pending_bios_int = cpu.pending_bios_int;
    core.pending_bios_int_valid = cpu.pending_bios_int_valid;
    core.a20_enabled = cpu.a20_enabled;
    core.irq13_pending = cpu.irq13_pending;

    apply_segment_to_core(&cpu.es, &mut core.segments.es);
    apply_segment_to_core(&cpu.cs, &mut core.segments.cs);
    apply_segment_to_core(&cpu.ss, &mut core.segments.ss);
    apply_segment_to_core(&cpu.ds, &mut core.segments.ds);
    apply_segment_to_core(&cpu.fs, &mut core.segments.fs);
    apply_segment_to_core(&cpu.gs, &mut core.segments.gs);

    core.fpu.fcw = cpu.fpu.fcw;
    core.fpu.fsw = cpu.fpu.fsw;
    core.fpu.ftw = cpu.fpu.ftw;
    core.fpu.top = cpu.fpu.top;
    core.fpu.fop = cpu.fpu.fop;
    core.fpu.fip = cpu.fpu.fip;
    core.fpu.fdp = cpu.fpu.fdp;
    core.fpu.fcs = cpu.fpu.fcs;
    core.fpu.fds = cpu.fpu.fds;
    core.fpu.st = cpu.fpu.st;

    core.sse.mxcsr = cpu.mxcsr;
    core.sse.xmm = cpu.xmm;
}

pub fn apply_mmu_state_to_cpu_core(mmu: &MmuState, core: &mut CoreCpuState) {
    core.control.cr0 = mmu.cr0;
    core.control.cr2 = mmu.cr2;
    core.control.cr3 = mmu.cr3;
    core.control.cr4 = mmu.cr4;
    core.control.cr8 = mmu.cr8;

    core.debug.dr[0] = mmu.dr0;
    core.debug.dr[1] = mmu.dr1;
    core.debug.dr[2] = mmu.dr2;
    core.debug.dr[3] = mmu.dr3;
    core.debug.dr6 = mmu.dr6;
    core.debug.dr7 = mmu.dr7;

    core.msr.efer = mmu.efer;
    core.msr.star = mmu.star;
    core.msr.lstar = mmu.lstar;
    core.msr.cstar = mmu.cstar;
    core.msr.fmask = mmu.sfmask;
    core.msr.sysenter_cs = mmu.sysenter_cs;
    core.msr.sysenter_eip = mmu.sysenter_eip;
    core.msr.sysenter_esp = mmu.sysenter_esp;
    core.msr.fs_base = mmu.fs_base;
    core.msr.gs_base = mmu.gs_base;
    core.msr.kernel_gs_base = mmu.kernel_gs_base;
    core.msr.apic_base = mmu.apic_base;
    core.msr.tsc = mmu.tsc;

    core.tables.gdtr.base = mmu.gdtr_base;
    core.tables.gdtr.limit = mmu.gdtr_limit;
    core.tables.idtr.base = mmu.idtr_base;
    core.tables.idtr.limit = mmu.idtr_limit;
    apply_segment_to_core(&mmu.ldtr, &mut core.tables.ldtr);
    apply_segment_to_core(&mmu.tr, &mut core.tables.tr);
}

fn segment_from_core(seg: &aero_cpu_core::state::Segment) -> SegmentState {
    SegmentState {
        selector: seg.selector,
        base: seg.base,
        limit: seg.limit,
        access: seg.access,
    }
}

fn apply_segment_to_core(src: &SegmentState, dst: &mut aero_cpu_core::state::Segment) {
    dst.selector = src.selector;
    dst.base = src.base;
    dst.limit = src.limit;
    dst.access = src.access;
}

fn fxsave64_bytes(
    fpu: &aero_cpu_core::fpu::FpuState,
    sse: &aero_cpu_core::sse_state::SseState,
) -> [u8; 512] {
    let mut out = [0u8; 512];

    out[0..2].copy_from_slice(&fpu.fcw.to_le_bytes());

    let fsw = fpu.fsw_with_top();
    out[2..4].copy_from_slice(&fsw.to_le_bytes());

    out[4] = fpu.ftw as u8;
    // out[5] reserved.
    out[6..8].copy_from_slice(&fpu.fop.to_le_bytes());

    out[8..16].copy_from_slice(&fpu.fip.to_le_bytes());
    out[16..24].copy_from_slice(&fpu.fdp.to_le_bytes());

    out[24..28].copy_from_slice(&sse.mxcsr.to_le_bytes());
    out[28..32].copy_from_slice(&MXCSR_MASK.to_le_bytes());

    for (i, reg) in fpu.st.iter().enumerate() {
        let start = 32 + i * 16;
        out[start..start + 16].copy_from_slice(&canonicalize_st(*reg).to_le_bytes());
    }

    for i in 0..16 {
        let start = 160 + i * 16;
        out[start..start + 16].copy_from_slice(&sse.xmm[i].to_le_bytes());
    }

    out
}
