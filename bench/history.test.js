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

test("normaliseBenchResult supports tools/perf raw.json format", () => {
  const { scenarios, environment } = normaliseBenchResult({
    meta: {
      nodeVersion: "v20.0.0",
      os: { platform: "linux", arch: "x64" },
    },
    benchmarks: [
      {
        name: "chromium_startup_ms",
        unit: "ms",
        samples: [10, 20, 30],
      },
    ],
  });

  assert.equal(environment.node, "v20.0.0");
  assert.equal(environment.platform, "linux");
  assert.equal(environment.arch, "x64");

  assert.equal(scenarios.browser.metrics.chromium_startup_ms.better, "lower");
  assert.equal(scenarios.browser.metrics.chromium_startup_ms.samples.n, 3);
  assert.equal(scenarios.browser.metrics.chromium_startup_ms.value, 20);
});

test("normaliseBenchResult supports scenario runner report.json format", () => {
  const { scenarios } = normaliseBenchResult({
    scenarioId: "system_boot",
    status: "ok",
    metrics: [{ id: "boot_time_ms", unit: "ms", value: 1234 }],
  });

  assert.equal(scenarios.system_boot.metrics.boot_time_ms.value, 1234);
  assert.equal(scenarios.system_boot.metrics.boot_time_ms.unit, "ms");
  assert.equal(scenarios.system_boot.metrics.boot_time_ms.better, "lower");
  assert.equal(scenarios.system_boot.metrics.boot_time_ms.samples.n, 1);
  assert.equal(scenarios.system_boot.metrics.boot_time_ms.samples.stdev, 0);
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
