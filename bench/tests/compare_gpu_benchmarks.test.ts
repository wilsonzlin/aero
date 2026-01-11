import test from "node:test";
import assert from "node:assert/strict";
import { compareGpuBenchmarks } from "../../scripts/compare_gpu_benchmarks.ts";

test("compareGpuBenchmarks fails on regressions beyond threshold", () => {
  const baseline = {
    meta: { gitSha: "base", iterations: 3 },
    summary: {
      scenarios: {
        vbe_lfb_blit: {
          name: "VBE LFB full-screen blit",
          status: "ok",
          metrics: {
            frameTimeMsP95: { median: 100, p50: 100, mean: 100, stdev: 0, cv: 0.1, p95: 100, n: 3 },
          },
        },
      },
    },
  };

  const current = {
    meta: { gitSha: "head", iterations: 3 },
    summary: {
      scenarios: {
        vbe_lfb_blit: {
          name: "VBE LFB full-screen blit",
          status: "ok",
          metrics: {
            frameTimeMsP95: { median: 120, p50: 120, mean: 120, stdev: 0, cv: 0.1, p95: 120, n: 3 },
          },
        },
      },
    },
  };

  const result = compareGpuBenchmarks({ baseline, current, thresholdPct: 15, cvThreshold: 0.5 });
  assert.equal(result.status, "fail");
  assert.equal(result.hasRegression, true);

  const row = result.rows.find((r) => r.scenarioId === "vbe_lfb_blit" && r.metric === "frameTimeMsP95");
  assert.ok(row, "expected a comparison row for frameTimeMsP95");
  assert.equal(row.regression, true);
  assert.equal(row.unstable, false);
  assert.ok(row.deltaPct != null && row.deltaPct > 0.15);
});

test("compareGpuBenchmarks returns unstable on extreme coefficient-of-variation", () => {
  const baseline = {
    meta: { gitSha: "base", iterations: 3 },
    summary: {
      scenarios: {
        vbe_lfb_blit: {
          name: "VBE LFB full-screen blit",
          status: "ok",
          metrics: {
            frameTimeMsP95: { median: 100, p50: 100, mean: 100, stdev: 0, cv: 0.1, p95: 100, n: 3 },
          },
        },
      },
    },
  };

  const current = {
    meta: { gitSha: "head", iterations: 3 },
    summary: {
      scenarios: {
        vbe_lfb_blit: {
          name: "VBE LFB full-screen blit",
          status: "ok",
          metrics: {
            frameTimeMsP95: { median: 100, p50: 100, mean: 100, stdev: 0, cv: 0.9, p95: 100, n: 3 },
          },
        },
      },
    },
  };

  const result = compareGpuBenchmarks({ baseline, current, thresholdPct: 15, cvThreshold: 0.5 });
  assert.equal(result.status, "unstable");
  assert.equal(result.isUnstable, true);

  const row = result.rows.find((r) => r.scenarioId === "vbe_lfb_blit" && r.metric === "frameTimeMsP95");
  assert.ok(row, "expected a comparison row for frameTimeMsP95");
  assert.equal(row.unstable, true);
  assert.equal(row.regression, false);
});

