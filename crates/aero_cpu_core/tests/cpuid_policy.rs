use aero_cpu_core::assist::AssistContext;
use aero_cpu_core::cpuid::{
    bits, cpuid, CpuFeatureOverrides, CpuFeatureSet, CpuFeatures, CpuProfile, CpuTopology,
};
use aero_cpu_core::mem::FlatTestBus;
use aero_cpu_core::msr;
use aero_cpu_core::state::{CpuMode, CpuState, CR0_PE};
use aero_cpu_core::AssistReason;
use aero_x86::Register;

const CODE_BASE: u64 = 0x100;

fn exec_wrmsr(
    ctx: &mut AssistContext,
    state: &mut CpuState,
    bus: &mut FlatTestBus,
    msr: u32,
    val: u64,
) {
    // `WRMSR` (0F 30) reads the MSR index from ECX and the value from EDX:EAX.
    state.write_reg(Register::ECX, msr as u64);
    state.write_reg(Register::EAX, val as u32 as u64);
    state.write_reg(Register::EDX, (val >> 32) as u32 as u64);
    state.set_rip(CODE_BASE);
    bus.load(CODE_BASE, &[0x0F, 0x30]);
    aero_cpu_core::assist::handle_assist(ctx, state, bus, AssistReason::Msr).unwrap();
}

#[test]
fn win7_minimum_profile_sets_required_bits() {
    let features = CpuFeatures::default();

    let leaf1 = cpuid(&features, 1, 0);
    assert_ne!(leaf1.edx & bits::LEAF1_EDX_SSE2, 0);
    assert_ne!(leaf1.edx & bits::LEAF1_EDX_PAE, 0);
    assert_ne!(leaf1.edx & bits::LEAF1_EDX_APIC, 0);
    assert_ne!(leaf1.edx & bits::LEAF1_EDX_TSC, 0);
    assert_ne!(leaf1.ecx & bits::LEAF1_ECX_CX16, 0);

    let ext1 = cpuid(&features, 0x8000_0001, 0);
    assert_ne!(ext1.edx & bits::EXT1_EDX_NX, 0);
    assert_ne!(ext1.edx & bits::EXT1_EDX_SYSCALL, 0);
    assert_ne!(ext1.edx & bits::EXT1_EDX_LM, 0);
}

#[test]
fn msr_efer_masks_nxe_when_cpuid_nx_is_disabled() {
    let features = CpuFeatures::from_profile(
        CpuProfile::Win7Minimum,
        CpuFeatureSet::win7_minimum(),
        CpuFeatureOverrides {
            force_disable: CpuFeatureSet {
                ext1_edx: bits::EXT1_EDX_NX,
                ..CpuFeatureSet::default()
            },
            ..CpuFeatureOverrides::default()
        },
        CpuTopology::default(),
    )
    .unwrap();

    assert_eq!(cpuid(&features, 0x8000_0001, 0).edx & bits::EXT1_EDX_NX, 0);

    let mut ctx = AssistContext {
        features,
        ..AssistContext::default()
    };
    let mut state = CpuState::new(CpuMode::Bit32);
    // Keep `CpuState::mode` coherent with `update_mode()` calls performed by the assist handler.
    state.control.cr0 |= CR0_PE;
    state.segments.cs.selector = 0x08; // CPL0 for WRMSR.
    let mut bus = FlatTestBus::new(0x1000);

    exec_wrmsr(
        &mut ctx,
        &mut state,
        &mut bus,
        msr::IA32_EFER,
        msr::EFER_NXE,
    );
    assert_eq!(state.msr.efer & msr::EFER_NXE, 0);
}

#[test]
fn msr_efer_masks_sce_when_cpuid_syscall_is_disabled() {
    let features = CpuFeatures::from_profile(
        CpuProfile::Win7Minimum,
        CpuFeatureSet::win7_minimum(),
        CpuFeatureOverrides {
            force_disable: CpuFeatureSet {
                ext1_edx: bits::EXT1_EDX_SYSCALL,
                ..CpuFeatureSet::default()
            },
            ..CpuFeatureOverrides::default()
        },
        CpuTopology::default(),
    )
    .unwrap();

    assert_eq!(
        cpuid(&features, 0x8000_0001, 0).edx & bits::EXT1_EDX_SYSCALL,
        0
    );

    let mut ctx = AssistContext {
        features,
        ..AssistContext::default()
    };
    let mut state = CpuState::new(CpuMode::Bit32);
    state.control.cr0 |= CR0_PE;
    state.segments.cs.selector = 0x08; // CPL0 for WRMSR.
    let mut bus = FlatTestBus::new(0x1000);

    exec_wrmsr(
        &mut ctx,
        &mut state,
        &mut bus,
        msr::IA32_EFER,
        msr::EFER_SCE,
    );
    assert_eq!(state.msr.efer & msr::EFER_SCE, 0);
}

#[test]
fn optimized_profile_only_exposes_implemented_extra_bits() {
    // When the emulator implements the extra bits, optimized profile can expose them.
    let features = CpuFeatures::from_profile(
        CpuProfile::Optimized,
        CpuFeatureSet::optimized_mask(),
        CpuFeatureOverrides::default(),
        CpuTopology::default(),
    )
    .unwrap();

    let leaf1 = cpuid(&features, 1, 0);
    assert_ne!(leaf1.ecx & bits::LEAF1_ECX_SSE3, 0);
    assert_ne!(leaf1.ecx & bits::LEAF1_ECX_SSE42, 0);
    assert_ne!(leaf1.ecx & bits::LEAF1_ECX_POPCNT, 0);

    // But if the emulator doesn't implement them, they must stay off even in optimized mode.
    let features = CpuFeatures::from_profile(
        CpuProfile::Optimized,
        CpuFeatureSet::win7_minimum(),
        CpuFeatureOverrides::default(),
        CpuTopology::default(),
    )
    .unwrap();

    let leaf1 = cpuid(&features, 1, 0);
    assert_eq!(leaf1.ecx & bits::LEAF1_ECX_SSE3, 0);
    assert_eq!(leaf1.ecx & bits::LEAF1_ECX_SSE42, 0);
    assert_eq!(leaf1.ecx & bits::LEAF1_ECX_POPCNT, 0);
}
