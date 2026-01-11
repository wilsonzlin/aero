//! Performance counters and derived statistics.
//!
//! # Instruction counting semantics
//! `instructions_executed` counts **retired guest architectural instructions**
//! (i.e. what the guest ISA considers an instruction), not internal micro-ops.
//!
//! In particular, x86 string/`REP*` instructions still retire as *one*
//! architectural instruction even though they may iterate many times. Those
//! iterations can be tracked separately via [`rep_iterations`].

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Thread-safe totals that can be sampled from outside a CPU worker thread.
///
/// Writes are performed in coarse batches via [`PerfWorker`] to keep overhead
/// minimal in hot execution loops.
#[derive(Debug, Default)]
pub struct PerfCounters {
    instructions_executed: AtomicU64,
    rep_iterations: AtomicU64,
}

impl PerfCounters {
    pub fn new() -> Self {
        Self::default()
    }

    /// Total number of retired guest architectural instructions observed by
    /// external samplers.
    ///
    /// Note: this value lags behind the CPU worker's exact total by up to the
    /// worker's batching threshold.
    pub fn instructions_executed(&self) -> u64 {
        self.instructions_executed.load(Ordering::Relaxed)
    }

    /// Total number of `REP*` iterations ("micro-ops") observed by external
    /// samplers.
    pub fn rep_iterations(&self) -> u64 {
        self.rep_iterations.load(Ordering::Relaxed)
    }

    pub fn snapshot(&self) -> PerfSnapshot {
        PerfSnapshot {
            instructions_executed: self.instructions_executed(),
            rep_iterations: self.rep_iterations(),
        }
    }
}

/// A snapshot of counters at a single point in time.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Default)]
pub struct PerfSnapshot {
    pub instructions_executed: u64,
    pub rep_iterations: u64,
}

impl PerfSnapshot {
    pub fn delta_since(self, earlier: PerfSnapshot) -> PerfDelta {
        PerfDelta {
            instructions_executed: self.instructions_executed - earlier.instructions_executed,
            rep_iterations: self.rep_iterations - earlier.rep_iterations,
        }
    }
}

/// Delta between two [`PerfSnapshot`]s.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Default)]
pub struct PerfDelta {
    pub instructions_executed: u64,
    pub rep_iterations: u64,
}

/// A per-worker, low-overhead batching frontend for [`PerfCounters`].
///
/// Hot paths update local counters with non-atomic operations and flush to the
/// shared atomics once the threshold is reached.
pub struct PerfWorker {
    shared: Arc<PerfCounters>,
    local_instructions: u64,
    local_rep_iterations: u64,
    flush_threshold: u64,
    last_frame: Option<(u64, PerfSnapshot)>,
    benchmark_start: Option<PerfSnapshot>,
}

impl PerfWorker {
    /// Default flush threshold (in retired guest instructions).
    ///
    /// This keeps the overhead of atomic operations negligible while still
    /// keeping exported counters reasonably fresh.
    pub const DEFAULT_FLUSH_THRESHOLD: u64 = 4096;

    pub fn new(shared: Arc<PerfCounters>) -> Self {
        Self::with_flush_threshold(shared, Self::DEFAULT_FLUSH_THRESHOLD)
    }

    pub fn with_flush_threshold(shared: Arc<PerfCounters>, flush_threshold: u64) -> Self {
        assert!(flush_threshold > 0);
        Self {
            shared,
            local_instructions: 0,
            local_rep_iterations: 0,
            flush_threshold,
            last_frame: None,
            benchmark_start: None,
        }
    }

    pub fn shared(&self) -> &Arc<PerfCounters> {
        &self.shared
    }

    /// Exact per-worker lifetime totals, including unflushed local counts.
    pub fn lifetime_snapshot(&self) -> PerfSnapshot {
        PerfSnapshot {
            instructions_executed: self.shared.instructions_executed() + self.local_instructions,
            rep_iterations: self.shared.rep_iterations() + self.local_rep_iterations,
        }
    }

    /// Retire `n` guest architectural instructions (interpreter: `n=1`,
    /// JIT: `n=block_instruction_count`).
    #[inline(always)]
    pub fn retire_instructions(&mut self, n: u64) {
        self.local_instructions += n;
        if self.local_instructions >= self.flush_threshold {
            self.shared
                .instructions_executed
                .fetch_add(self.local_instructions, Ordering::Relaxed);
            self.local_instructions = 0;
        }
    }

    /// Record `n` `REP*` iterations ("micro-ops"). This does **not** affect
    /// `instructions_executed`.
    #[inline(always)]
    pub fn add_rep_iterations(&mut self, n: u64) {
        self.local_rep_iterations += n;
        if self.local_rep_iterations >= self.flush_threshold {
            self.shared
                .rep_iterations
                .fetch_add(self.local_rep_iterations, Ordering::Relaxed);
            self.local_rep_iterations = 0;
        }
    }

    /// Flush pending local counts into the shared atomics.
    pub fn flush(&mut self) {
        if self.local_instructions != 0 {
            self.shared
                .instructions_executed
                .fetch_add(self.local_instructions, Ordering::Relaxed);
            self.local_instructions = 0;
        }
        if self.local_rep_iterations != 0 {
            self.shared
                .rep_iterations
                .fetch_add(self.local_rep_iterations, Ordering::Relaxed);
            self.local_rep_iterations = 0;
        }
    }

    /// Mark the start of a new frame, returning the delta since the previous
    /// frame boundary.
    pub fn begin_frame(&mut self, frame_id: u64) -> PerfDelta {
        let now = self.lifetime_snapshot();
        let delta = match self.last_frame {
            Some((_prev_frame_id, prev)) => now.delta_since(prev),
            None => PerfDelta::default(),
        };
        self.last_frame = Some((frame_id, now));
        delta
    }

    /// Begin a benchmark run, resetting the per-run baseline.
    pub fn begin_benchmark(&mut self) {
        self.benchmark_start = Some(self.lifetime_snapshot());
    }

    /// Return the instruction delta since the last [`begin_benchmark`] call.
    pub fn benchmark_delta(&self) -> Option<PerfDelta> {
        self.benchmark_start
            .map(|start| self.lifetime_snapshot().delta_since(start))
    }

    /// End a benchmark run and return the total delta for the run.
    pub fn end_benchmark(&mut self) -> Option<PerfDelta> {
        let delta = self.benchmark_delta();
        self.benchmark_start = None;
        delta
    }
}

impl Drop for PerfWorker {
    fn drop(&mut self) {
        self.flush();
    }
}

/// Compute MIPS (million instructions per second) from a delta.
pub fn compute_mips(instructions_delta: u64, wall_time_delta: Duration) -> f64 {
    let secs = wall_time_delta.as_secs_f64();
    if secs == 0.0 {
        return 0.0;
    }
    (instructions_delta as f64) / secs / 1_000_000.0
}

/// Rolling window statistics for MIPS.
pub struct MipsWindow {
    samples: VecDeque<f64>,
    capacity: usize,
    sum: f64,
}

impl MipsWindow {
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0);
        Self {
            samples: VecDeque::with_capacity(capacity),
            capacity,
            sum: 0.0,
        }
    }

    pub fn len(&self) -> usize {
        self.samples.len()
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    pub fn push(&mut self, sample_mips: f64) {
        if self.samples.len() == self.capacity {
            if let Some(old) = self.samples.pop_front() {
                self.sum -= old;
            }
        }
        self.samples.push_back(sample_mips);
        self.sum += sample_mips;
    }

    pub fn avg(&self) -> f64 {
        if self.samples.is_empty() {
            return 0.0;
        }
        self.sum / (self.samples.len() as f64)
    }

    pub fn p95(&self) -> f64 {
        if self.samples.is_empty() {
            return 0.0;
        }
        let mut sorted: Vec<f64> = self.samples.iter().copied().collect();
        sorted.sort_by(|a, b| a.total_cmp(b));
        let idx = ((sorted.len() as f64) * 0.95).ceil() as usize;
        let idx = idx.saturating_sub(1).min(sorted.len() - 1);
        sorted[idx]
    }
}

/// Helper to compute deltas and MIPS samples from periodic snapshots.
pub struct PerfMonitor {
    prev_snapshot: PerfSnapshot,
    prev_wall_time: Instant,
    mips_window: MipsWindow,
}

#[derive(Debug, Clone)]
pub struct PerfMonitorSample {
    pub delta: PerfDelta,
    pub wall_time_delta: Duration,
    pub mips: f64,
    pub mips_avg: f64,
    pub mips_p95: f64,
}

impl PerfMonitor {
    pub fn new(window_capacity: usize, initial_snapshot: PerfSnapshot, now: Instant) -> Self {
        Self {
            prev_snapshot: initial_snapshot,
            prev_wall_time: now,
            mips_window: MipsWindow::new(window_capacity),
        }
    }

    pub fn update(&mut self, snapshot: PerfSnapshot, now: Instant) -> PerfMonitorSample {
        let delta = snapshot.delta_since(self.prev_snapshot);
        let wall_time_delta = now.duration_since(self.prev_wall_time);
        let mips = compute_mips(delta.instructions_executed, wall_time_delta);
        self.mips_window.push(mips);

        self.prev_snapshot = snapshot;
        self.prev_wall_time = now;

        PerfMonitorSample {
            delta,
            wall_time_delta,
            mips,
            mips_avg: self.mips_window.avg(),
            mips_p95: self.mips_window.p95(),
        }
    }
}
