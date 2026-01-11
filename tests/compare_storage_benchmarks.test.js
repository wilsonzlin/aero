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
    sequential_write: { mean_mb_per_s: 100 },
    sequential_read: { mean_mb_per_s: 150 },
    random_read_4k: { mean_p50_ms: 5, mean_p95_ms: 10 },
  };

  const current = {
    sequential_write: { mean_mb_per_s: 105 },
    sequential_read: { mean_mb_per_s: 149 },
    random_read_4k: { mean_p50_ms: 5.1, mean_p95_ms: 9.9 },
  };

  const result = compareStorageBenchmarks({ baseline, current, thresholdPct: 15 });
  assert.equal(result.pass, true);
  assert.equal(result.comparisons.length, 4);
});

test("compareStorageBenchmarks: detects optional random_write regressions when present", () => {
  const baseline = {
    sequential_write: { mean_mb_per_s: 100 },
    sequential_read: { mean_mb_per_s: 150 },
    random_read_4k: { mean_p50_ms: 5, mean_p95_ms: 10 },
    random_write_4k: { mean_p95_ms: 20 },
  };

  const current = {
    sequential_write: { mean_mb_per_s: 100 },
    sequential_read: { mean_mb_per_s: 150 },
    random_read_4k: { mean_p50_ms: 5, mean_p95_ms: 10 },
    random_write_4k: { mean_p95_ms: 26 },
  };

  const result = compareStorageBenchmarks({ baseline, current, thresholdPct: 15 });
  assert.equal(result.pass, false);
  assert.ok(result.comparisons.find((c) => c.metric === "random_write_4k.mean_p95_ms")?.regression);
});

