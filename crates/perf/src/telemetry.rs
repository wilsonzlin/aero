use crate::jit::{
    format_hud_line, totals_to_export, JitExport, JitMetrics, JitMetricsTotals, JitRollingExport,
};
use serde::Serialize;
use std::sync::{Arc, Mutex};
use std::time::Instant;

#[derive(Debug)]
pub struct Telemetry {
    pub jit: Arc<JitMetrics>,
    jit_rolling: Mutex<Option<JitRollingState>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TelemetrySnapshot {
    pub jit: JitExport,
}

impl TelemetrySnapshot {
    pub fn jit_hud_line(&self) -> String {
        format_hud_line(&self.jit)
    }
}

#[derive(Debug, Clone, Copy)]
struct JitRollingState {
    at: Instant,
    totals: JitMetricsTotals,
}

impl Telemetry {
    pub fn new(jit_enabled: bool) -> Self {
        Self {
            jit: Arc::new(JitMetrics::new(jit_enabled)),
            jit_rolling: Mutex::new(None),
        }
    }

    pub fn snapshot(&self) -> TelemetrySnapshot {
        let now = Instant::now();
        let totals = self.jit.snapshot_totals();
        let rolling = self.snapshot_jit_rolling(now, totals);
        TelemetrySnapshot {
            jit: totals_to_export(self.jit.enabled(), totals, rolling),
        }
    }

    pub fn snapshot_json_pretty(&self) -> String {
        serde_json::to_string_pretty(&self.snapshot()).expect("telemetry snapshot must serialize")
    }

    fn snapshot_jit_rolling(&self, now: Instant, totals: JitMetricsTotals) -> JitRollingExport {
        let mut guard = self
            .jit_rolling
            .lock()
            .expect("telemetry jit rolling lock poisoned");

        let Some(prev) = *guard else {
            *guard = Some(JitRollingState { at: now, totals });
            return JitRollingExport {
                window_ms: 0,
                cache_hit_rate: 0.0,
                compile_ms_per_s: 0.0,
                blocks_compiled_per_s: 0.0,
            };
        };

        let window = now.duration_since(prev.at);
        *guard = Some(JitRollingState { at: now, totals });

        let window_ms = window.as_millis() as u64;
        let window_s = window.as_secs_f64();
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
        let delta_lookups = delta_hits + delta_misses;
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
