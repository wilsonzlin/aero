use aero_cpu_core::cpuid::{
    bits, cpuid, CpuFeatureOverrides, CpuFeatureSet, CpuFeatures, CpuProfile, CpuTopology,
};

#[test]
fn cpuid_leaf1_htt_cleared_for_single_logical_processor() {
    let features = CpuFeatures::from_profile(
        CpuProfile::Win7Minimum,
        CpuFeatureSet::win7_minimum(),
        CpuFeatureOverrides::default(),
        CpuTopology {
            cores_per_package: 1,
            threads_per_core: 1,
            apic_id: 0,
            x2apic_id: 0,
        },
    )
    .unwrap();

    let leaf1 = cpuid(&features, 1, 0);
    assert_eq!(leaf1.edx & bits::LEAF1_EDX_HTT, 0);
}

#[test]
fn cpuid_leaf1_htt_set_for_multiple_logical_processors() {
    let features = CpuFeatures::from_profile(
        CpuProfile::Win7Minimum,
        CpuFeatureSet::win7_minimum(),
        CpuFeatureOverrides::default(),
        CpuTopology {
            cores_per_package: 2,
            threads_per_core: 1,
            apic_id: 0,
            x2apic_id: 0,
        },
    )
    .unwrap();

    let leaf1 = cpuid(&features, 1, 0);
    assert_ne!(leaf1.edx & bits::LEAF1_EDX_HTT, 0);
}
