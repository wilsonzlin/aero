import test from "node:test";
import assert from "node:assert/strict";

import { compareMetric, compareStorageBenchmarks } from "../scripts/compare_storage_benchmarks.ts";

test("compareMetric: throughput (higher is better) flags regressions", () => {
  const threshold = 0.15;
  assert.deepEqual(compareMetric({ baseline: 100, current: 90, better: "higher", threshold }), {
    deltaPct: -0.1,
    regression: false,
  });
  assert.deepEqual(compareMetric({ baseline: 100, current: 80, better: "higher", threshold }), {
    deltaPct: -0.2,
    regression: true,
  });
});

test("compareMetric: latency (lower is better) flags regressions", () => {
  const threshold = 0.15;
  assert.deepEqual(compareMetric({ baseline: 10, current: 11, better: "lower", threshold }), {
    deltaPct: 0.1,
    regression: false,
  });
  assert.deepEqual(compareMetric({ baseline: 10, current: 12, better: "lower", threshold }), {
    deltaPct: 0.2,
    regression: true,
  });
});

test("compareStorageBenchmarks: skips optional random_write when absent", () => {
  const baseline = {
    backend: "opfs",
    api_mode: "async",
    sequential_write: { mean_mb_per_s: 100 },
    sequential_read: { mean_mb_per_s: 150 },
    random_read_4k: { mean_p50_ms: 5, mean_p95_ms: 10 },
  };

  const current = {
    backend: "opfs",
    api_mode: "async",
    sequential_write: { mean_mb_per_s: 105 },
    sequential_read: { mean_mb_per_s: 149 },
    random_read_4k: { mean_p50_ms: 5.1, mean_p95_ms: 9.9 },
  };

  const result = compareStorageBenchmarks({ baseline, current, thresholdPct: 15 });
  assert.equal(result.pass, true);
  assert.equal(result.comparisons.length, 4);
  assert.equal(result.metadataComparisons.length, 2);
});

test("compareStorageBenchmarks: detects optional random_write regressions when present", () => {
  const baseline = {
    backend: "opfs",
    api_mode: "async",
    sequential_write: { mean_mb_per_s: 100 },
    sequential_read: { mean_mb_per_s: 150 },
    random_read_4k: { mean_p50_ms: 5, mean_p95_ms: 10 },
    random_write_4k: { mean_p95_ms: 20 },
  };

  const current = {
    backend: "opfs",
    api_mode: "async",
    sequential_write: { mean_mb_per_s: 100 },
    sequential_read: { mean_mb_per_s: 150 },
    random_read_4k: { mean_p50_ms: 5, mean_p95_ms: 10 },
    random_write_4k: { mean_p95_ms: 26 },
  };

  const result = compareStorageBenchmarks({ baseline, current, thresholdPct: 15 });
  assert.equal(result.pass, false);
  assert.ok(result.comparisons.find((c) => c.metric === "random_write_4k.mean_p95_ms")?.regression);
});

test("compareStorageBenchmarks: fails when backend regresses (opfs -> indexeddb)", () => {
  const baseline = {
    backend: "opfs",
    api_mode: "sync_access_handle",
    sequential_write: { mean_mb_per_s: 100 },
    sequential_read: { mean_mb_per_s: 150 },
    random_read_4k: { mean_p50_ms: 5, mean_p95_ms: 10 },
  };

  const current = {
    backend: "indexeddb",
    api_mode: "async",
    sequential_write: { mean_mb_per_s: 100 },
    sequential_read: { mean_mb_per_s: 150 },
    random_read_4k: { mean_p50_ms: 5, mean_p95_ms: 10 },
  };

  const result = compareStorageBenchmarks({ baseline, current, thresholdPct: 15 });
  assert.equal(result.pass, false);
  assert.ok(result.metadataComparisons.find((c) => c.field === "backend")?.regression);
});

test("compareStorageBenchmarks: fails when OPFS api_mode regresses (sync -> async)", () => {
  const baseline = {
    backend: "opfs",
    api_mode: "sync_access_handle",
    config: { seq_total_mb: 32, seq_chunk_mb: 4, seq_runs: 2, warmup_mb: 8, random_ops: 500, random_runs: 2, random_space_mb: 4, random_seed: 1337, include_random_write: false },
    sequential_write: { mean_mb_per_s: 100 },
    sequential_read: { mean_mb_per_s: 150 },
    random_read_4k: { mean_p50_ms: 5, mean_p95_ms: 10 },
  };

  const current = {
    backend: "opfs",
    api_mode: "async",
    config: { seq_total_mb: 32, seq_chunk_mb: 4, seq_runs: 2, warmup_mb: 8, random_ops: 500, random_runs: 2, random_space_mb: 4, random_seed: 1337, include_random_write: false },
    sequential_write: { mean_mb_per_s: 100 },
    sequential_read: { mean_mb_per_s: 150 },
    random_read_4k: { mean_p50_ms: 5, mean_p95_ms: 10 },
  };

  const result = compareStorageBenchmarks({ baseline, current, thresholdPct: 15 });
  assert.equal(result.pass, false);
  assert.ok(result.metadataComparisons.find((c) => c.field === "api_mode")?.regression);
});

test("compareStorageBenchmarks: fails when benchmark config changes", () => {
  const baseline = {
    backend: "opfs",
    api_mode: "async",
    config: { seq_total_mb: 32, seq_chunk_mb: 4, seq_runs: 2, warmup_mb: 8, random_ops: 500, random_runs: 2, random_space_mb: 4, random_seed: 1337, include_random_write: false },
    sequential_write: { mean_mb_per_s: 100 },
    sequential_read: { mean_mb_per_s: 150 },
    random_read_4k: { mean_p50_ms: 5, mean_p95_ms: 10 },
  };

  const current = {
    backend: "opfs",
    api_mode: "async",
    config: { seq_total_mb: 32, seq_chunk_mb: 4, seq_runs: 2, warmup_mb: 8, random_ops: 750, random_runs: 2, random_space_mb: 4, random_seed: 1337, include_random_write: false },
    sequential_write: { mean_mb_per_s: 100 },
    sequential_read: { mean_mb_per_s: 150 },
    random_read_4k: { mean_p50_ms: 5, mean_p95_ms: 10 },
  };

  const result = compareStorageBenchmarks({ baseline, current, thresholdPct: 15 });
  assert.equal(result.pass, false);
  assert.ok(result.metadataComparisons.find((c) => c.field === "config")?.regression);
});
