use perf::jit::{JitTier, JitTier2Pass};
use perf::telemetry::Telemetry;
use std::time::Duration;

#[test]
fn jit_metrics_disabled_stays_zero() {
    let telemetry = Telemetry::new(false);

    telemetry.jit.record_cache_hit();
    telemetry.jit.record_cache_miss();
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

    let snapshot = telemetry.snapshot();
    assert!(!snapshot.jit.enabled);
    assert_eq!(snapshot.jit.totals.tier1.blocks_compiled, 0);
    assert_eq!(snapshot.jit.totals.tier2.blocks_compiled, 0);
    assert_eq!(snapshot.jit.totals.cache.lookup_hit, 0);
    assert_eq!(snapshot.jit.totals.cache.lookup_miss, 0);
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
    telemetry.jit.record_block_compiled(JitTier::Tier1);
    telemetry
        .jit
        .add_compile_time(JitTier::Tier1, Duration::from_millis(7));

    telemetry.jit.record_cache_hit();
    telemetry.jit.record_block_compiled(JitTier::Tier2);
    telemetry
        .jit
        .add_compile_time(JitTier::Tier2, Duration::from_millis(3));
    telemetry
        .jit
        .add_tier2_pass_time(JitTier2Pass::Dce, Duration::from_millis(2));
    telemetry.jit.record_guard_fail();

    let snapshot = telemetry.snapshot();
    assert!(snapshot.jit.enabled);
    assert_eq!(snapshot.jit.totals.cache.capacity_bytes, 1024);
    assert_eq!(snapshot.jit.totals.cache.used_bytes, 128);
    assert_eq!(snapshot.jit.totals.cache.lookup_miss, 1);
    assert_eq!(snapshot.jit.totals.cache.lookup_hit, 1);
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
