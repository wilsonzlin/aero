import test from "node:test";
import assert from "node:assert/strict";

import { buildStorageCompareResult, renderStorageCompareMarkdown } from "../compare.ts";

const suiteThresholds = {
  metrics: {
    sequential_write_mb_per_s: { better: "higher", maxRegressionPct: 0.15, extremeCvThreshold: 0.5 },
    sequential_read_mb_per_s: { better: "higher", maxRegressionPct: 0.15, extremeCvThreshold: 0.5 },
    random_read_4k_p95_ms: { better: "lower", maxRegressionPct: 0.15, extremeCvThreshold: 0.5 },
    random_write_4k_p95_ms: { better: "lower", maxRegressionPct: 0.15, informational: true },
  },
};

function makeReport(overrides: any = {}) {
  return {
    run_id: "run",
    backend: "opfs",
    api_mode: "sync_access_handle",
    config: {
      seq_total_mb: 32,
      seq_chunk_mb: 4,
      seq_runs: 2,
      warmup_mb: 8,
      random_ops: 500,
      random_runs: 2,
      random_space_mb: 4,
      random_seed: 1337,
      include_random_write: false,
    },
    sequential_write: { runs: [{ mb_per_s: 100 }, { mb_per_s: 100 }] },
    sequential_read: { runs: [{ mb_per_s: 150 }, { mb_per_s: 150 }] },
    random_read_4k: { runs: [{ p95_ms: 10 }, { p95_ms: 10 }] },
    random_write_4k: null,
    warnings: [],
    ...overrides,
  };
}

test("buildStorageCompareResult: uses median-of-runs samples", () => {
  const baseline = makeReport({
    sequential_write: { runs: [{ mb_per_s: 1 }, { mb_per_s: 2 }, { mb_per_s: 100 }] },
    warnings: ["baseline warning"],
  });
  const candidate = makeReport({
    sequential_write: { runs: [{ mb_per_s: 1 }, { mb_per_s: 2 }, { mb_per_s: 100 }] },
    warnings: ["candidate warning"],
  });

  const { result, contextChecks, baselineWarnings, candidateWarnings } = buildStorageCompareResult({
    baseline,
    candidate,
    thresholdsFile: "bench/perf_thresholds.json",
    profileName: "pr-smoke",
    suiteThresholds,
    overrideMaxRegressionPct: null,
    overrideExtremeCv: null,
  });

  const row = result.comparisons.find((c: any) => c.metric === "sequential_write_mb_per_s");
  assert.ok(row, "expected sequential_write_mb_per_s to be compared");
  assert.equal(row.baseline.value, 2);
  assert.equal(row.candidate.value, 2);

  const markdown = renderStorageCompareMarkdown({
    result,
    contextChecks,
    baselineWarnings,
    candidateWarnings,
  });
  assert.ok(markdown.includes("## Context"));
  assert.ok(markdown.includes("## Warnings"));
  assert.ok(markdown.includes("Baseline warnings:"));
  assert.ok(markdown.includes("Candidate warnings:"));
});

test("buildStorageCompareResult: flags regressions beyond threshold", () => {
  const baseline = makeReport({
    sequential_write: { runs: [{ mb_per_s: 100 }, { mb_per_s: 100 }, { mb_per_s: 100 }] },
  });
  const candidate = makeReport({
    sequential_write: { runs: [{ mb_per_s: 80 }, { mb_per_s: 80 }, { mb_per_s: 80 }] },
  });

  const { result } = buildStorageCompareResult({
    baseline,
    candidate,
    thresholdsFile: "bench/perf_thresholds.json",
    profileName: "pr-smoke",
    suiteThresholds,
    overrideMaxRegressionPct: null,
    overrideExtremeCv: null,
  });

  assert.equal(result.status, "regression");
});

test("buildStorageCompareResult: marks extreme variance as unstable", () => {
  const baseline = makeReport({
    sequential_write: { runs: [{ mb_per_s: 100 }, { mb_per_s: 100 }] },
  });
  const candidate = makeReport({
    sequential_write: { runs: [{ mb_per_s: 1 }, { mb_per_s: 1000 }] },
  });

  const { result } = buildStorageCompareResult({
    baseline,
    candidate,
    thresholdsFile: "bench/perf_thresholds.json",
    profileName: "pr-smoke",
    suiteThresholds,
    overrideMaxRegressionPct: null,
    overrideExtremeCv: null,
  });

  assert.equal(result.status, "unstable");
});

test("buildStorageCompareResult: marks apples-to-oranges comparisons as unstable (backend regression)", () => {
  const baseline = makeReport({ backend: "opfs" });
  const candidate = makeReport({ backend: "indexeddb", api_mode: "async" });

  const { result, contextChecks } = buildStorageCompareResult({
    baseline,
    candidate,
    thresholdsFile: "bench/perf_thresholds.json",
    profileName: "pr-smoke",
    suiteThresholds,
    overrideMaxRegressionPct: null,
    overrideExtremeCv: null,
  });

  assert.equal(result.status, "unstable");
  assert.ok(contextChecks.find((c) => c.field === "backend" && c.status === "fail"));
});
