use serde::Serialize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JitTier {
    Tier1,
    Tier2,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JitTier2Pass {
    ConstFold,
    Dce,
    RegAlloc,
}

impl JitTier2Pass {
    pub const fn name(self) -> &'static str {
        match self {
            JitTier2Pass::ConstFold => "const_fold",
            JitTier2Pass::Dce => "dce",
            JitTier2Pass::RegAlloc => "regalloc",
        }
    }
}

#[derive(Debug)]
pub struct JitMetrics {
    enabled: bool,

    // Tier compile totals.
    tier1_compile_ns_total: AtomicU64,
    tier2_compile_ns_total: AtomicU64,

    // Tier 2 pass breakdown totals.
    tier2_pass_const_fold_ns_total: AtomicU64,
    tier2_pass_dce_ns_total: AtomicU64,
    tier2_pass_regalloc_ns_total: AtomicU64,

    // Tier distribution.
    tier1_blocks_compiled_total: AtomicU64,
    tier2_blocks_compiled_total: AtomicU64,

    // Code cache.
    cache_capacity_bytes: AtomicU64,
    cache_used_bytes: AtomicU64,
    cache_lookup_hit_total: AtomicU64,
    cache_lookup_miss_total: AtomicU64,

    // Deopts / guard failures.
    deopt_total: AtomicU64,
    guard_fail_total: AtomicU64,
}

impl JitMetrics {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            tier1_compile_ns_total: AtomicU64::new(0),
            tier2_compile_ns_total: AtomicU64::new(0),
            tier2_pass_const_fold_ns_total: AtomicU64::new(0),
            tier2_pass_dce_ns_total: AtomicU64::new(0),
            tier2_pass_regalloc_ns_total: AtomicU64::new(0),
            tier1_blocks_compiled_total: AtomicU64::new(0),
            tier2_blocks_compiled_total: AtomicU64::new(0),
            cache_capacity_bytes: AtomicU64::new(0),
            cache_used_bytes: AtomicU64::new(0),
            cache_lookup_hit_total: AtomicU64::new(0),
            cache_lookup_miss_total: AtomicU64::new(0),
            deopt_total: AtomicU64::new(0),
            guard_fail_total: AtomicU64::new(0),
        }
    }

    #[inline]
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    #[inline]
    pub fn record_cache_hit(&self) {
        if !self.enabled {
            return;
        }
        self.cache_lookup_hit_total.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn record_cache_miss(&self) {
        if !self.enabled {
            return;
        }
        self.cache_lookup_miss_total.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn record_block_compiled(&self, tier: JitTier) {
        if !self.enabled {
            return;
        }
        match tier {
            JitTier::Tier1 => {
                self.tier1_blocks_compiled_total
                    .fetch_add(1, Ordering::Relaxed);
            }
            JitTier::Tier2 => {
                self.tier2_blocks_compiled_total
                    .fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    #[inline]
    pub fn add_compile_time(&self, tier: JitTier, duration: Duration) {
        if !self.enabled {
            return;
        }
        let ns = duration.as_nanos() as u64;
        match tier {
            JitTier::Tier1 => {
                self.tier1_compile_ns_total.fetch_add(ns, Ordering::Relaxed);
            }
            JitTier::Tier2 => {
                self.tier2_compile_ns_total.fetch_add(ns, Ordering::Relaxed);
            }
        }
    }

    #[inline]
    pub fn add_tier2_pass_time(&self, pass: JitTier2Pass, duration: Duration) {
        if !self.enabled {
            return;
        }
        let ns = duration.as_nanos() as u64;
        match pass {
            JitTier2Pass::ConstFold => {
                self.tier2_pass_const_fold_ns_total
                    .fetch_add(ns, Ordering::Relaxed);
            }
            JitTier2Pass::Dce => {
                self.tier2_pass_dce_ns_total
                    .fetch_add(ns, Ordering::Relaxed);
            }
            JitTier2Pass::RegAlloc => {
                self.tier2_pass_regalloc_ns_total
                    .fetch_add(ns, Ordering::Relaxed);
            }
        }
    }

    #[inline]
    pub fn set_cache_capacity_bytes(&self, bytes: u64) {
        if !self.enabled {
            return;
        }
        self.cache_capacity_bytes.store(bytes, Ordering::Relaxed);
    }

    #[inline]
    pub fn set_cache_used_bytes(&self, bytes: u64) {
        if !self.enabled {
            return;
        }
        self.cache_used_bytes.store(bytes, Ordering::Relaxed);
    }

    #[inline]
    pub fn record_deopt(&self) {
        if !self.enabled {
            return;
        }
        self.deopt_total.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn record_guard_fail(&self) {
        if !self.enabled {
            return;
        }
        self.guard_fail_total.fetch_add(1, Ordering::Relaxed);
    }

    pub fn snapshot_totals(&self) -> JitMetricsTotals {
        JitMetricsTotals {
            cache_lookup_hit_total: self.cache_lookup_hit_total.load(Ordering::Relaxed),
            cache_lookup_miss_total: self.cache_lookup_miss_total.load(Ordering::Relaxed),
            tier1_blocks_compiled_total: self.tier1_blocks_compiled_total.load(Ordering::Relaxed),
            tier2_blocks_compiled_total: self.tier2_blocks_compiled_total.load(Ordering::Relaxed),
            tier1_compile_ns_total: self.tier1_compile_ns_total.load(Ordering::Relaxed),
            tier2_compile_ns_total: self.tier2_compile_ns_total.load(Ordering::Relaxed),
            tier2_pass_const_fold_ns_total: self
                .tier2_pass_const_fold_ns_total
                .load(Ordering::Relaxed),
            tier2_pass_dce_ns_total: self.tier2_pass_dce_ns_total.load(Ordering::Relaxed),
            tier2_pass_regalloc_ns_total: self.tier2_pass_regalloc_ns_total.load(Ordering::Relaxed),
            deopt_total: self.deopt_total.load(Ordering::Relaxed),
            guard_fail_total: self.guard_fail_total.load(Ordering::Relaxed),
            code_cache_capacity_bytes: self.cache_capacity_bytes.load(Ordering::Relaxed),
            code_cache_used_bytes: self.cache_used_bytes.load(Ordering::Relaxed),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct JitMetricsTotals {
    pub cache_lookup_hit_total: u64,
    pub cache_lookup_miss_total: u64,
    pub tier1_blocks_compiled_total: u64,
    pub tier2_blocks_compiled_total: u64,
    pub tier1_compile_ns_total: u64,
    pub tier2_compile_ns_total: u64,
    pub tier2_pass_const_fold_ns_total: u64,
    pub tier2_pass_dce_ns_total: u64,
    pub tier2_pass_regalloc_ns_total: u64,
    pub deopt_total: u64,
    pub guard_fail_total: u64,
    pub code_cache_capacity_bytes: u64,
    pub code_cache_used_bytes: u64,
}

impl JitMetricsTotals {
    pub const fn blocks_compiled_total(&self) -> u64 {
        self.tier1_blocks_compiled_total + self.tier2_blocks_compiled_total
    }

    pub const fn compile_ns_total(&self) -> u64 {
        self.tier1_compile_ns_total + self.tier2_compile_ns_total
    }

    pub const fn cache_lookups_total(&self) -> u64 {
        self.cache_lookup_hit_total + self.cache_lookup_miss_total
    }

    pub fn cache_hit_rate(&self) -> f64 {
        let total = self.cache_lookups_total();
        if total == 0 {
            return 0.0;
        }
        self.cache_lookup_hit_total as f64 / total as f64
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct JitExport {
    pub enabled: bool,
    pub totals: JitTotalsExport,
    pub rolling: JitRollingExport,
}

#[derive(Debug, Clone, Serialize)]
pub struct JitTotalsExport {
    pub tier1: JitTierExport,
    pub tier2: JitTier2Export,
    pub cache: JitCacheExport,
    pub deopt: JitDeoptExport,
}

#[derive(Debug, Clone, Serialize)]
pub struct JitTierExport {
    pub blocks_compiled: u64,
    pub compile_ms: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct JitTier2Export {
    pub blocks_compiled: u64,
    pub compile_ms: f64,
    pub passes_ms: JitTier2PassesExport,
}

#[derive(Debug, Clone, Serialize)]
pub struct JitTier2PassesExport {
    pub const_fold: f64,
    pub dce: f64,
    pub regalloc: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct JitCacheExport {
    pub lookup_hit: u64,
    pub lookup_miss: u64,
    pub capacity_bytes: u64,
    pub used_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct JitDeoptExport {
    pub count: u64,
    pub guard_fail: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct JitRollingExport {
    pub window_ms: u64,
    pub cache_hit_rate: f64,
    pub compile_ms_per_s: f64,
    pub blocks_compiled_per_s: f64,
}

pub fn totals_to_export(
    enabled: bool,
    totals: JitMetricsTotals,
    rolling: JitRollingExport,
) -> JitExport {
    JitExport {
        enabled,
        totals: JitTotalsExport {
            tier1: JitTierExport {
                blocks_compiled: totals.tier1_blocks_compiled_total,
                compile_ms: ns_to_ms(totals.tier1_compile_ns_total),
            },
            tier2: JitTier2Export {
                blocks_compiled: totals.tier2_blocks_compiled_total,
                compile_ms: ns_to_ms(totals.tier2_compile_ns_total),
                passes_ms: JitTier2PassesExport {
                    const_fold: ns_to_ms(totals.tier2_pass_const_fold_ns_total),
                    dce: ns_to_ms(totals.tier2_pass_dce_ns_total),
                    regalloc: ns_to_ms(totals.tier2_pass_regalloc_ns_total),
                },
            },
            cache: JitCacheExport {
                lookup_hit: totals.cache_lookup_hit_total,
                lookup_miss: totals.cache_lookup_miss_total,
                capacity_bytes: totals.code_cache_capacity_bytes,
                used_bytes: totals.code_cache_used_bytes,
            },
            deopt: JitDeoptExport {
                count: totals.deopt_total,
                guard_fail: totals.guard_fail_total,
            },
        },
        rolling,
    }
}

pub fn ns_to_ms(ns: u64) -> f64 {
    ns as f64 / 1_000_000.0
}

pub fn format_hud_line(jit: &JitExport) -> String {
    let hit_rate_pct = jit.rolling.cache_hit_rate * 100.0;
    format!(
        "jit: hit_rate={hit_rate_pct:.1}% blocks(t1={t1_blocks},t2={t2_blocks}) compile_ms(t1={t1_ms:.1},t2={t2_ms:.1}) compile_ms/s={compile_rate:.2} deopt={deopt} guard_fail={guard_fail} cache={used}/{cap}B",
        t1_blocks = jit.totals.tier1.blocks_compiled,
        t2_blocks = jit.totals.tier2.blocks_compiled,
        t1_ms = jit.totals.tier1.compile_ms,
        t2_ms = jit.totals.tier2.compile_ms,
        compile_rate = jit.rolling.compile_ms_per_s,
        deopt = jit.totals.deopt.count,
        guard_fail = jit.totals.deopt.guard_fail,
        used = jit.totals.cache.used_bytes,
        cap = jit.totals.cache.capacity_bytes,
    )
}
