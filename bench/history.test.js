import assert from "node:assert/strict";
import test from "node:test";

import { computeStats, formatDelta, normaliseBenchResult } from "./history.js";

test("computeStats calculates mean + stdev + cv", () => {
  const stats = computeStats([1, 2, 3, 4]);
  assert.equal(stats.n, 4);
  assert.equal(stats.min, 1);
  assert.equal(stats.max, 4);
  assert.equal(stats.mean, 2.5);
  assert.ok(stats.stdev > 1);
  assert.ok(stats.cv > 0);
});

test("normaliseBenchResult produces metric summaries", () => {
  const { scenarios } = normaliseBenchResult({
    schemaVersion: 1,
    meta: { node: "v20.0.0", platform: "linux", arch: "x64" },
    scenarios: {
      startup: {
        metrics: {
          startup_ms: {
            unit: "ms",
            better: "lower",
            samples: [10, 20, 30],
          },
        },
      },
    },
  });

  assert.equal(scenarios.startup.metrics.startup_ms.unit, "ms");
  assert.equal(scenarios.startup.metrics.startup_ms.better, "lower");
  assert.equal(scenarios.startup.metrics.startup_ms.samples.n, 3);
  assert.equal(scenarios.startup.metrics.startup_ms.value, 20);
});

test("formatDelta respects metric directionality", () => {
  assert.equal(
    formatDelta({ prev: 100, next: 110, better: "higher", unit: "ops/s" }).className,
    "improvement",
  );
  assert.equal(
    formatDelta({ prev: 100, next: 110, better: "lower", unit: "ms" }).className,
    "regression",
  );
});
