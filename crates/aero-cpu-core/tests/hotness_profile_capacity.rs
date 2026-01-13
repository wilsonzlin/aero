use aero_cpu_core::jit::profile::HotnessProfile;

#[test]
fn hotness_profile_is_capacity_bounded() {
    let cap = 8;
    let mut profile = HotnessProfile::new_with_capacity(/*threshold=*/ 1_000, cap);

    // Record hits for far more distinct RIPs than the profile capacity; the profile must stay
    // bounded regardless of guest behavior.
    for rip in 0..(cap as u64 * 10) {
        profile.record_hit(rip, /*has_compiled_block=*/ false);
        assert!(
            profile.len() <= cap,
            "profile grew beyond cap: len={} cap={}",
            profile.len(),
            cap
        );
    }
}

#[test]
fn hot_rip_requests_compile_once_even_near_capacity() {
    let cap = 8;
    let threshold = 3;
    let mut profile = HotnessProfile::new_with_capacity(threshold, cap);

    // Fill the profile close to capacity with cold RIPs.
    for rip in 0..(cap as u64 - 1) {
        profile.record_hit(rip, /*has_compiled_block=*/ false);
    }

    // Drive one RIP over the hot threshold.
    let hot_rip = 0xdead_beef;
    let mut compile_requests = 0;
    for _ in 0..threshold {
        if profile.record_hit(hot_rip, /*has_compiled_block=*/ false) {
            compile_requests += 1;
        }
    }

    // Thrash the profile with many more distinct RIPs; the hot RIP should stay pinned as
    // "requested" and must not produce duplicate compile requests.
    for rip in 0..100u64 {
        profile.record_hit(0x1_0000 + rip, /*has_compiled_block=*/ false);
    }

    for _ in 0..10 {
        if profile.record_hit(hot_rip, /*has_compiled_block=*/ false) {
            compile_requests += 1;
        }
    }

    assert_eq!(compile_requests, 1);
}
