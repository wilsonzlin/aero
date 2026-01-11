#![cfg(feature = "legacy-interp")]

use aero_cpu_core::cpuid::{bits, cpuid, CpuFeatureOverrides, CpuFeatureSet, CpuFeatures, CpuProfile, CpuTopology};
use aero_cpu_core::msr;
use aero_cpu_core::system::Cpu;

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

    let mut cpu = Cpu::new(features);
    cpu.cs = 0x8; // CPL0 for WRMSR

    cpu.wrmsr_value(msr::IA32_EFER, msr::EFER_NXE).unwrap();
    assert_eq!(cpu.rdmsr_value(msr::IA32_EFER).unwrap() & msr::EFER_NXE, 0);
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

    let mut cpu = Cpu::new(features);
    cpu.cs = 0x8; // CPL0 for WRMSR

    cpu.wrmsr_value(msr::IA32_EFER, msr::EFER_SCE).unwrap();
    assert_eq!(cpu.rdmsr_value(msr::IA32_EFER).unwrap() & msr::EFER_SCE, 0);
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
