import test from "node:test";
import assert from "node:assert/strict";

import compareModule from "../bench/lib/compare.cjs";

const { compareResults, computeDeltaPct, computeRegressionPct, resolveThresholdRule } = compareModule;

test("computeDeltaPct: returns signed percentage delta", () => {
  assert.equal(computeDeltaPct(100, 110), 10);
  assert.equal(computeDeltaPct(100, 90), -10);
});

test("computeRegressionPct: higher is better", () => {
  assert.equal(computeRegressionPct("higher", 100, 110), 0);
  assert.equal(computeRegressionPct("higher", 100, 90), 0.1);
});

test("computeRegressionPct: lower is better", () => {
  assert.equal(computeRegressionPct("lower", 100, 90), 0);
  assert.equal(computeRegressionPct("lower", 100, 110), 0.1);
});

test("resolveThresholdRule: scenario overrides metric overrides default", () => {
  const profile = {
    default: { maxRegressionPct: 0.2 },
    metrics: { json_parse_ops_s: { maxRegressionPct: 0.1 } },
    scenarios: {
      startup: { metrics: { startup_ms: { maxRegressionPct: 0.05 } } },
    },
  };

  assert.deepEqual(resolveThresholdRule(profile, "startup", "startup_ms"), { maxRegressionPct: 0.05 });
  assert.deepEqual(resolveThresholdRule(profile, "microbench", "json_parse_ops_s"), { maxRegressionPct: 0.1 });
  assert.deepEqual(resolveThresholdRule(profile, "microbench", "unknown"), { maxRegressionPct: 0.2 });
});

test("compareResults: detects regressions and respects informational metrics", () => {
  const baseline = {
    schemaVersion: 1,
    scenarios: {
      microbench: {
        metrics: {
          json_parse_ops_s: { unit: "ops/s", better: "higher", samples: [100, 100, 100] },
          rss_mb: { unit: "MB", better: "lower", samples: [100, 100, 100] },
        },
      },
    },
  };

  const current = {
    schemaVersion: 1,
    scenarios: {
      microbench: {
        metrics: {
          json_parse_ops_s: { unit: "ops/s", better: "higher", samples: [70, 70, 70] },
          rss_mb: { unit: "MB", better: "lower", samples: [200, 200, 200] },
        },
      },
    },
  };

  const thresholds = {
    schemaVersion: 1,
    profiles: {
      pr: {
        default: { maxRegressionPct: 0.1, varianceCvWarn: 0.1 },
        metrics: {
          rss_mb: { informational: true, maxRegressionPct: 0.1 },
        },
      },
    },
  };

  const result = compareResults({ baseline, current, thresholds, profileName: "pr" });
  assert.equal(result.summary.total, 2);
  assert.equal(result.summary.regressions, 1);
  assert.equal(result.summary.informationalRegressions, 1);
});

test("compareResults: absolute thresholds trigger regression", () => {
  const baseline = {
    schemaVersion: 1,
    scenarios: {
      startup: {
        metrics: {
          startup_ms: { unit: "ms", better: "lower", samples: [100, 100, 100] },
        },
      },
    },
  };

  const current = {
    schemaVersion: 1,
    scenarios: {
      startup: {
        metrics: {
          startup_ms: { unit: "ms", better: "lower", samples: [150, 150, 150] },
        },
      },
    },
  };

  const thresholds = {
    schemaVersion: 1,
    profiles: {
      pr: {
        metrics: {
          startup_ms: { maxValue: 120 },
        },
      },
    },
  };

  const result = compareResults({ baseline, current, thresholds, profileName: "pr" });
  assert.equal(result.summary.regressions, 1);
  assert.equal(result.comparisons[0].breaches[0].type, "maxValue");
});

test("compareResults: flags variance warnings when cv exceeds threshold", () => {
  const baseline = {
    schemaVersion: 1,
    scenarios: {
      microbench: {
        metrics: {
          json_parse_ops_s: { unit: "ops/s", better: "higher", samples: [100, 100, 100] },
        },
      },
    },
  };

  const current = {
    schemaVersion: 1,
    scenarios: {
      microbench: {
        metrics: {
          json_parse_ops_s: { unit: "ops/s", better: "higher", samples: [1, 100, 1000] },
        },
      },
    },
  };

  const thresholds = {
    schemaVersion: 1,
    profiles: {
      pr: {
        default: { varianceCvWarn: 0.05 },
      },
    },
  };

  const result = compareResults({ baseline, current, thresholds, profileName: "pr" });
  assert.equal(result.summary.varianceWarnings, 1);
  assert.ok(result.comparisons[0].varianceWarnings.length > 0);
});

