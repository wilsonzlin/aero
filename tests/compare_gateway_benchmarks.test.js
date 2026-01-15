import assert from "node:assert/strict";
import test from "node:test";

import { compareGatewayBenchmarks } from "../scripts/compare_gateway_benchmarks.js";

function baseGatewayPayload() {
  return {
    meta: {
      mode: "smoke",
      nodeVersion: "v20.0.0",
      platform: "linux",
      arch: "x64",
      doh: { durationSeconds: 3 },
    },
    tcpProxy: {
      rttMs: { n: 1, min: 1, p50: 1, p90: 1, p99: 1, max: 1, mean: 1, stdev: 0, cv: 0 },
      throughput: { bytes: 1024, seconds: 1, mibPerSecond: 1, stats: { n: 1, min: 1, max: 1, mean: 1 } },
    },
    doh: {
      qps: 1000,
      qpsStats: { n: 1, min: 1000, max: 1000, mean: 1000, stdev: 0, cv: 0 },
      latencyMs: { p50: 1, p90: 2, p99: 3, n: 1, min: 1, max: 3, mean: 2, stdev: 0, cv: 0 },
      cache: { hits: 1, misses: 0, hitRatio: 1 },
      raw: {},
    },
  };
}

test("compareGatewayBenchmarks marks missing candidate metrics as unstable", () => {
  const baselineRaw = baseGatewayPayload();
  const candidateRaw = baseGatewayPayload();
  delete candidateRaw.doh.qps;

  const result = compareGatewayBenchmarks({
    baselineRaw,
    candidateRaw,
    suiteThresholds: {
      metrics: {
        doh_qps: { better: "higher", maxRegressionPct: 0.15, extremeCvThreshold: 0.5 },
      },
    },
    thresholdsFile: "bench/perf_thresholds.json",
    profileName: "pr-smoke",
  });

  assert.equal(result.status, "unstable");
  assert.equal(result.comparisons[0].status, "missing_candidate");
});

test("compareGatewayBenchmarks marks missing baseline required metrics as unstable", () => {
  const baselineRaw = baseGatewayPayload();
  delete baselineRaw.doh.qps;
  const candidateRaw = baseGatewayPayload();

  const result = compareGatewayBenchmarks({
    baselineRaw,
    candidateRaw,
    suiteThresholds: {
      metrics: {
        doh_qps: { better: "higher", maxRegressionPct: 0.15, extremeCvThreshold: 0.5 },
      },
    },
    thresholdsFile: "bench/perf_thresholds.json",
    profileName: "pr-smoke",
  });

  assert.equal(result.status, "unstable");
  assert.equal(result.comparisons[0].status, "missing_baseline");
});

test("compareGatewayBenchmarks allows missing baseline informational metrics", () => {
  const baselineRaw = baseGatewayPayload();
  delete baselineRaw.doh.qps;
  const candidateRaw = baseGatewayPayload();

  const result = compareGatewayBenchmarks({
    baselineRaw,
    candidateRaw,
    suiteThresholds: {
      metrics: {
        doh_qps: { better: "higher", maxRegressionPct: 0.15, informational: true },
      },
    },
    thresholdsFile: "bench/perf_thresholds.json",
    profileName: "pr-smoke",
  });

  assert.equal(result.status, "pass");
  assert.equal(result.comparisons[0].status, "missing_baseline");
  assert.equal(result.comparisons[0].unstable, false);
});

test("compareGatewayBenchmarks reports regressions for required metrics", () => {
  const baselineRaw = baseGatewayPayload();
  const candidateRaw = baseGatewayPayload();
  candidateRaw.doh.qps = 800;
  candidateRaw.doh.qpsStats.mean = 800;
  candidateRaw.doh.qpsStats.min = 800;
  candidateRaw.doh.qpsStats.max = 800;

  const result = compareGatewayBenchmarks({
    baselineRaw,
    candidateRaw,
    suiteThresholds: {
      metrics: {
        doh_qps: { better: "higher", maxRegressionPct: 0.15, extremeCvThreshold: 0.5 },
      },
    },
    thresholdsFile: "bench/perf_thresholds.json",
    profileName: "pr-smoke",
  });

  assert.equal(result.status, "regression");
  assert.equal(result.comparisons[0].status, "regression");
});
