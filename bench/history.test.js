import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";

import { appendHistoryEntry, computeStats, formatDelta, normaliseBenchResult } from "./history.js";

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

test("normaliseBenchResult supports aero-gpu-bench report format", () => {
  const { scenarios, environment } = normaliseBenchResult({
    schemaVersion: 1,
    tool: "aero-gpu-bench",
    startedAt: "2025-01-01T00:00:00Z",
    finishedAt: "2025-01-01T00:00:01Z",
    environment: { userAgent: "UA", webgpu: false, webgl2: true },
    scenarios: {
      vga_text_scroll: {
        id: "vga_text_scroll",
        name: "VGA text scroll",
        status: "ok",
        durationMs: 123,
        params: {},
        telemetry: { droppedFrames: 0 },
        derived: {
          fpsAvg: 60,
          frameTimeMsP50: 16,
          frameTimeMsP95: 20,
          presentLatencyMsP95: 3,
          shaderTranslationMsMean: 1,
          shaderCompilationMsMean: 2,
          pipelineCacheHitRate: 0.5,
          textureUploadMBpsAvg: 10,
        },
      },
    },
  });

  assert.equal(environment.userAgent, "UA");
  assert.equal(environment.webgpu, false);
  assert.equal(environment.webgl2, true);

  assert.equal(scenarios["gpu/vga_text_scroll"].metrics.fps_avg.value, 60);
  assert.equal(scenarios["gpu/vga_text_scroll"].metrics.fps_avg.unit, "fps");
  assert.equal(scenarios["gpu/vga_text_scroll"].metrics.fps_avg.better, "higher");
  assert.equal(scenarios["gpu/vga_text_scroll"].metrics.pipeline_cache_hit_rate_pct.value, 50);
  assert.equal(scenarios["gpu/vga_text_scroll"].metrics.dropped_frames.value, 0);
});

test("appendHistoryEntry merges multiple inputs for the same entry id", () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "aero-history-"));
  const historyPath = path.join(tmpDir, "history.json");
  const perfPath = path.join(tmpDir, "perf.json");
  const gpuPath = path.join(tmpDir, "gpu.json");

  fs.writeFileSync(
    perfPath,
    JSON.stringify(
      {
        meta: { nodeVersion: "v20.0.0", os: { platform: "linux", arch: "x64" } },
        benchmarks: [{ name: "chromium_startup_ms", unit: "ms", samples: [10, 20, 30] }],
      },
      null,
      2,
    ),
  );
  fs.writeFileSync(
    gpuPath,
    JSON.stringify(
      {
        schemaVersion: 1,
        tool: "aero-gpu-bench",
        startedAt: "2025-01-01T00:00:00Z",
        finishedAt: "2025-01-01T00:00:01Z",
        environment: { userAgent: "UA", webgpu: false, webgl2: true },
        scenarios: {
          vga_text_scroll: {
            id: "vga_text_scroll",
            name: "VGA text scroll",
            status: "ok",
            durationMs: 123,
            params: {},
            telemetry: { droppedFrames: 0 },
            derived: { fpsAvg: 60 },
          },
        },
      },
      null,
      2,
    ),
  );

  const timestamp = "2025-01-01T00:00:00Z";
  const commit = "0123456789abcdef0123456789abcdef01234567";
  const repository = "wilsonzlin/aero";

  appendHistoryEntry({ historyPath, inputPath: perfPath, timestamp, commitSha: commit, repository });
  appendHistoryEntry({ historyPath, inputPath: gpuPath, timestamp, commitSha: commit, repository });

  const history = JSON.parse(fs.readFileSync(historyPath, "utf8"));
  const entryId = `${timestamp}-${commit}`;
  const entry = history.entries[entryId];
  assert.ok(entry);
  assert.ok(entry.scenarios.browser);
  assert.ok(entry.scenarios["gpu/vga_text_scroll"]);
  assert.equal(entry.environment.node, "v20.0.0");
  assert.equal(entry.environment.userAgent, "UA");
});

test("appendHistoryEntry rejects duplicate metric keys during merge", () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "aero-history-"));
  const historyPath = path.join(tmpDir, "history.json");
  const inputA = path.join(tmpDir, "a.json");
  const inputB = path.join(tmpDir, "b.json");

  fs.writeFileSync(
    inputA,
    JSON.stringify({ meta: {}, benchmarks: [{ name: "metric_ms", unit: "ms", samples: [1] }] }, null, 2),
  );
  fs.writeFileSync(
    inputB,
    JSON.stringify({ meta: {}, benchmarks: [{ name: "metric_ms", unit: "ms", samples: [2] }] }, null, 2),
  );

  const timestamp = "2025-01-01T00:00:00Z";
  const commit = "0123456789abcdef0123456789abcdef01234567";
  const repository = "wilsonzlin/aero";

  appendHistoryEntry({ historyPath, inputPath: inputA, timestamp, commitSha: commit, repository });

  assert.throws(() => {
    appendHistoryEntry({ historyPath, inputPath: inputB, timestamp, commitSha: commit, repository });
  }, /duplicate metric browser\.metric_ms/);
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
