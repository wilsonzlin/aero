use perf::jit::{ns_to_ms, JitMetricsTotals, JitTier, JitTier2Pass};
use perf::telemetry::Telemetry;
use std::time::Duration;

#[test]
fn jit_metrics_disabled_stays_zero() {
    let telemetry = Telemetry::new(false);

    telemetry.jit.record_cache_hit();
    telemetry.jit.record_cache_miss();
    telemetry.jit.record_cache_install();
    telemetry.jit.record_cache_evict(3);
    telemetry.jit.record_cache_invalidate();
    telemetry.jit.record_cache_stale_install_reject();
    telemetry.jit.record_compile_request();
    telemetry.jit.record_block_compiled(JitTier::Tier1);
    telemetry.jit.record_block_compiled(JitTier::Tier2);
    telemetry
        .jit
        .add_compile_time(JitTier::Tier1, Duration::from_millis(10));
    telemetry
        .jit
        .add_compile_time(JitTier::Tier2, Duration::from_millis(10));
    telemetry
        .jit
        .add_tier2_pass_time(JitTier2Pass::ConstFold, Duration::from_millis(5));
    telemetry.jit.set_cache_capacity_bytes(123);
    telemetry.jit.set_cache_used_bytes(456);
    telemetry.jit.record_deopt();
    telemetry.jit.record_guard_fail();

    let totals = telemetry.jit.snapshot_totals();
    assert_eq!(totals.cache_install_total, 0);
    assert_eq!(totals.cache_evict_total, 0);
    assert_eq!(totals.cache_invalidate_total, 0);
    assert_eq!(totals.cache_stale_install_reject_total, 0);
    assert_eq!(totals.compile_request_total, 0);

    let snapshot = telemetry.snapshot();
    assert!(!snapshot.jit.enabled);
    assert_eq!(snapshot.jit.totals.tier1.blocks_compiled, 0);
    assert_eq!(snapshot.jit.totals.tier2.blocks_compiled, 0);
    assert_eq!(snapshot.jit.totals.cache.lookup_hit, 0);
    assert_eq!(snapshot.jit.totals.cache.lookup_miss, 0);
    assert_eq!(snapshot.jit.totals.cache.install, 0);
    assert_eq!(snapshot.jit.totals.cache.evict, 0);
    assert_eq!(snapshot.jit.totals.cache.invalidate, 0);
    assert_eq!(snapshot.jit.totals.cache.stale_install_reject, 0);
    assert_eq!(snapshot.jit.totals.cache.compile_request, 0);
    assert_eq!(snapshot.jit.totals.cache.capacity_bytes, 0);
    assert_eq!(snapshot.jit.totals.cache.used_bytes, 0);
    assert_eq!(snapshot.jit.totals.tier1.compile_ms, 0.0);
    assert_eq!(snapshot.jit.totals.tier2.compile_ms, 0.0);
    assert_eq!(snapshot.jit.totals.deopt.count, 0);
    assert_eq!(snapshot.jit.totals.deopt.guard_fail, 0);
}

#[test]
fn jit_metrics_enabled_exports_nonzero_totals() {
    let telemetry = Telemetry::new(true);
    telemetry.jit.set_cache_capacity_bytes(1024);
    telemetry.jit.set_cache_used_bytes(128);

    telemetry.jit.record_cache_miss();
    telemetry.jit.record_cache_install();
    telemetry.jit.record_block_compiled(JitTier::Tier1);
    telemetry
        .jit
        .add_compile_time(JitTier::Tier1, Duration::from_millis(7));

    telemetry.jit.record_cache_hit();
    telemetry.jit.record_cache_evict(2);
    telemetry.jit.record_cache_invalidate();
    telemetry.jit.record_cache_stale_install_reject();
    telemetry.jit.record_compile_request();
    telemetry.jit.record_block_compiled(JitTier::Tier2);
    telemetry
        .jit
        .add_compile_time(JitTier::Tier2, Duration::from_millis(3));
    telemetry
        .jit
        .add_tier2_pass_time(JitTier2Pass::Dce, Duration::from_millis(2));
    telemetry.jit.record_guard_fail();

    let totals = telemetry.jit.snapshot_totals();
    assert_eq!(totals.cache_install_total, 1);
    assert_eq!(totals.cache_evict_total, 2);
    assert_eq!(totals.cache_invalidate_total, 1);
    assert_eq!(totals.cache_stale_install_reject_total, 1);
    assert_eq!(totals.compile_request_total, 1);

    let snapshot = telemetry.snapshot();
    assert!(snapshot.jit.enabled);
    assert_eq!(snapshot.jit.totals.cache.capacity_bytes, 1024);
    assert_eq!(snapshot.jit.totals.cache.used_bytes, 128);
    assert_eq!(snapshot.jit.totals.cache.lookup_miss, 1);
    assert_eq!(snapshot.jit.totals.cache.lookup_hit, 1);
    assert_eq!(snapshot.jit.totals.cache.install, 1);
    assert_eq!(snapshot.jit.totals.cache.evict, 2);
    assert_eq!(snapshot.jit.totals.cache.invalidate, 1);
    assert_eq!(snapshot.jit.totals.cache.stale_install_reject, 1);
    assert_eq!(snapshot.jit.totals.cache.compile_request, 1);
    assert_eq!(snapshot.jit.totals.tier1.blocks_compiled, 1);
    assert_eq!(snapshot.jit.totals.tier2.blocks_compiled, 1);
    assert!(snapshot.jit.totals.tier1.compile_ms > 0.0);
    assert!(snapshot.jit.totals.tier2.compile_ms > 0.0);
    assert_eq!(snapshot.jit.totals.deopt.count, 0);
    assert_eq!(snapshot.jit.totals.deopt.guard_fail, 1);
    assert!(snapshot.jit.totals.tier2.passes_ms.dce > 0.0);
}

#[test]
fn jit_metrics_rolling_reports_rates() {
    let telemetry = Telemetry::new(true);

    // Prime rolling window.
    let _ = telemetry.snapshot();

    telemetry.jit.record_cache_hit();
    telemetry.jit.record_cache_miss();
    telemetry.jit.record_block_compiled(JitTier::Tier1);
    telemetry
        .jit
        .add_compile_time(JitTier::Tier1, Duration::from_millis(5));

    std::thread::sleep(Duration::from_millis(10));

    let snapshot = telemetry.snapshot();
    assert!(snapshot.jit.rolling.window_ms > 0);
    assert!(snapshot.jit.rolling.compile_ms_per_s > 0.0);
    assert!(snapshot.jit.rolling.blocks_compiled_per_s > 0.0);
    assert!(snapshot.jit.rolling.cache_hit_rate >= 0.0);
    assert!(snapshot.jit.rolling.cache_hit_rate <= 1.0);
}

#[test]
fn jit_metrics_compile_time_saturates_on_overflow() {
    let telemetry = Telemetry::new(true);

    telemetry
        .jit
        .add_compile_time(JitTier::Tier1, Duration::from_secs(u64::MAX));
    telemetry
        .jit
        .add_compile_time(JitTier::Tier1, Duration::from_secs(1));

    let snapshot = telemetry.snapshot();
    assert_eq!(snapshot.jit.totals.tier1.compile_ms, ns_to_ms(u64::MAX));
}

#[test]
fn jit_metrics_totals_helpers_saturate_on_overflow() {
    let totals = JitMetricsTotals {
        cache_lookup_hit_total: u64::MAX,
        cache_lookup_miss_total: u64::MAX,
        cache_install_total: 0,
        cache_evict_total: 0,
        cache_invalidate_total: 0,
        cache_stale_install_reject_total: 0,
        compile_request_total: 0,
        tier1_blocks_compiled_total: u64::MAX,
        tier2_blocks_compiled_total: u64::MAX,
        tier1_compile_ns_total: u64::MAX,
        tier2_compile_ns_total: u64::MAX,
        tier2_pass_const_fold_ns_total: 0,
        tier2_pass_dce_ns_total: 0,
        tier2_pass_regalloc_ns_total: 0,
        deopt_total: 0,
        guard_fail_total: 0,
        code_cache_capacity_bytes: 0,
        code_cache_used_bytes: 0,
    };

    assert_eq!(totals.cache_lookups_total(), u64::MAX);
    assert_eq!(totals.blocks_compiled_total(), u64::MAX);
    assert_eq!(totals.compile_ns_total(), u64::MAX);
}
