import test from "node:test";
import assert from "node:assert/strict";

import { buildCompareResult, computeRegressionPct, exitCodeForStatus } from "../tools/perf/lib/compare_core.mjs";
import { pickThresholdProfile } from "../tools/perf/lib/thresholds.mjs";

test("computeRegressionPct: respects directionality", () => {
  assert.equal(computeRegressionPct("lower", 100, 110), 0.1);
  assert.equal(computeRegressionPct("lower", 100, 90), 0);

  assert.equal(computeRegressionPct("higher", 100, 90), 0.1);
  assert.equal(computeRegressionPct("higher", 100, 110), 0);
});

test("buildCompareResult: flags regressions when regressionPct >= maxRegressionPct", () => {
  const result = buildCompareResult({
    suite: "browser",
    profile: "pr-smoke",
    thresholdsFile: "bench/perf_thresholds.json",
    baselineMeta: { gitSha: "base" },
    candidateMeta: { gitSha: "head" },
    cases: [
      {
        scenario: "browser",
        metric: "microbench_ms",
        unit: "ms",
        better: "lower",
        threshold: { maxRegressionPct: 0.1 },
        baseline: { value: 100, cv: 0.05, n: 3 },
        candidate: { value: 110, cv: 0.05, n: 3 },
      },
    ],
  });

  assert.equal(result.status, "regression");
  assert.equal(result.summary.regressions, 1);
  assert.equal(exitCodeForStatus(result.status), 1);
});

test("buildCompareResult: flags extreme variance as unstable (exit code 2)", () => {
  const result = buildCompareResult({
    suite: "browser",
    profile: "pr-smoke",
    thresholdsFile: "bench/perf_thresholds.json",
    baselineMeta: { gitSha: "base" },
    candidateMeta: { gitSha: "head" },
    cases: [
      {
        scenario: "browser",
        metric: "microbench_ms",
        unit: "ms",
        better: "lower",
        threshold: { maxRegressionPct: 1, extremeCvThreshold: 0.5 },
        baseline: { value: 100, cv: 0.1, n: 3 },
        candidate: { value: 100, cv: 0.9, n: 3 },
      },
    ],
  });

  assert.equal(result.status, "unstable");
  assert.equal(result.summary.unstable, 1);
  assert.equal(exitCodeForStatus(result.status), 2);
});

test("pickThresholdProfile: defaults to pr-smoke when profileName is omitted", () => {
  const policy = {
    schemaVersion: 1,
    profiles: {
      "pr-smoke": {
        browser: { metrics: { microbench_ms: { better: "lower", maxRegressionPct: 0.1 } } },
      },
    },
  };

  const { name } = pickThresholdProfile(policy);
  assert.equal(name, "pr-smoke");
});

