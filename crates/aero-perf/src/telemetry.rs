use crate::jit::{
    format_hud_line, totals_to_export, JitExport, JitMetrics, JitMetricsTotals, JitRollingExport,
};
use serde::Serialize;
use std::sync::{Arc, Mutex};

#[cfg(not(target_arch = "wasm32"))]
#[inline]
fn duration_as_u64_ns(duration: std::time::Duration) -> u64 {
    duration.as_nanos().min(u64::MAX as u128) as u64
}

#[derive(Debug)]
pub struct Telemetry {
    pub jit: Arc<JitMetrics>,
    jit_rolling: Mutex<Option<JitRollingState>>,
    /// Native-only monotonic time anchor for [`Telemetry::snapshot`].
    ///
    /// WebAssembly builds should use [`Telemetry::snapshot_at`] and supply a monotonic timestamp
    /// (e.g. derived from `performance.now()` or an emulator-wide host clock abstraction).
    #[cfg(not(target_arch = "wasm32"))]
    start: std::time::Instant,
}

#[derive(Debug, Clone, Serialize)]
pub struct TelemetrySnapshot {
    pub jit: JitExport,
}

impl TelemetrySnapshot {
    pub fn jit_hud_line(&self) -> String {
        format_hud_line(&self.jit)
    }

    pub fn to_json_pretty(&self) -> String {
        serde_json::to_string_pretty(self).expect("telemetry snapshot must serialize")
    }
}

#[derive(Debug, Clone, Copy)]
struct JitRollingState {
    at_ns: u64,
    totals: JitMetricsTotals,
}

impl Telemetry {
    pub fn new(jit_enabled: bool) -> Self {
        Self {
            jit: Arc::new(JitMetrics::new(jit_enabled)),
            jit_rolling: Mutex::new(None),
            #[cfg(not(target_arch = "wasm32"))]
            start: std::time::Instant::now(),
        }
    }

    /// Snapshot telemetry using an embedder-supplied monotonic timestamp.
    ///
    /// The timestamp is interpreted as nanoseconds since an arbitrary origin, and is only used
    /// to compute rolling window deltas (rates/hit-rate). Totals are always monotonic counters.
    pub fn snapshot_at(&self, now_ns: u64) -> TelemetrySnapshot {
        let totals = self.jit.snapshot_totals();
        let rolling = self.snapshot_jit_rolling(now_ns, totals);
        TelemetrySnapshot {
            jit: totals_to_export(self.jit.enabled(), totals, rolling),
        }
    }

    /// Snapshot telemetry using an internal monotonic clock (native-only).
    ///
    /// On WebAssembly targets, this uses a fixed timestamp (`0`) and therefore produces rolling
    /// metrics of `0`. Use [`Telemetry::snapshot_at`] for meaningful rolling rates on wasm.
    pub fn snapshot(&self) -> TelemetrySnapshot {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let now_ns = duration_as_u64_ns(self.start.elapsed());
            self.snapshot_at(now_ns)
        }

        #[cfg(target_arch = "wasm32")]
        {
            self.snapshot_at(0)
        }
    }

    pub fn snapshot_json_pretty(&self) -> String {
        serde_json::to_string_pretty(&self.snapshot()).expect("telemetry snapshot must serialize")
    }

    fn snapshot_jit_rolling(&self, now_ns: u64, totals: JitMetricsTotals) -> JitRollingExport {
        let mut guard = self
            .jit_rolling
            .lock()
            .expect("telemetry jit rolling lock poisoned");

        let Some(prev) = *guard else {
            *guard = Some(JitRollingState {
                at_ns: now_ns,
                totals,
            });
            return JitRollingExport {
                window_ms: 0,
                cache_hit_rate: 0.0,
                compile_ms_per_s: 0.0,
                blocks_compiled_per_s: 0.0,
            };
        };

        let window_ns = now_ns.saturating_sub(prev.at_ns);
        *guard = Some(JitRollingState {
            at_ns: now_ns,
            totals,
        });

        let window_ms = window_ns / 1_000_000;
        let window_s = (window_ns as f64) / 1_000_000_000.0;
        if window_s <= 0.0 {
            return JitRollingExport {
                window_ms,
                cache_hit_rate: 0.0,
                compile_ms_per_s: 0.0,
                blocks_compiled_per_s: 0.0,
            };
        }

        let delta_hits = totals
            .cache_lookup_hit_total
            .saturating_sub(prev.totals.cache_lookup_hit_total);
        let delta_misses = totals
            .cache_lookup_miss_total
            .saturating_sub(prev.totals.cache_lookup_miss_total);
        let delta_lookups = delta_hits.saturating_add(delta_misses);
        let cache_hit_rate = if delta_lookups == 0 {
            0.0
        } else {
            delta_hits as f64 / delta_lookups as f64
        };

        let delta_compile_ns = totals
            .compile_ns_total()
            .saturating_sub(prev.totals.compile_ns_total());
        let compile_ms_per_s = (delta_compile_ns as f64 / 1_000_000.0) / window_s;

        let delta_blocks = totals
            .blocks_compiled_total()
            .saturating_sub(prev.totals.blocks_compiled_total());
        let blocks_compiled_per_s = delta_blocks as f64 / window_s;

        JitRollingExport {
            window_ms,
            cache_hit_rate,
            compile_ms_per_s,
            blocks_compiled_per_s,
        }
    }
}
