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

test("normaliseBenchResult infers throughput metrics as higher-is-better", () => {
  const { scenarios } = normaliseBenchResult({
    scenarioId: "storage_io",
    status: "ok",
    metrics: [{ id: "storage_seq_write_mb_per_s", unit: "MB/s", value: 123.4 }],
  });

  assert.equal(scenarios.storage_io.metrics.storage_seq_write_mb_per_s.better, "higher");
});

test("normaliseBenchResult supports storage_bench.json format", () => {
  const { scenarios, environment } = normaliseBenchResult({
    version: 1,
    run_id: "run-123",
    backend: "opfs",
    api_mode: "sync_access_handle",
    config: {},
    sequential_write: {
      runs: [
        { bytes: 1024, duration_ms: 10, mb_per_s: 100 },
        { bytes: 1024, duration_ms: 10, mb_per_s: 120 },
      ],
      mean_mb_per_s: 110,
      stdev_mb_per_s: 14,
    },
    sequential_read: {
      runs: [
        { bytes: 1024, duration_ms: 10, mb_per_s: 200 },
        { bytes: 1024, duration_ms: 10, mb_per_s: 180 },
      ],
      mean_mb_per_s: 190,
      stdev_mb_per_s: 14,
    },
    random_read_4k: {
      runs: [
        { ops: 10, block_bytes: 4096, min_ms: 1, max_ms: 2, mean_ms: 1.5, stdev_ms: 0.1, p50_ms: 1.4, p95_ms: 5 },
        { ops: 10, block_bytes: 4096, min_ms: 1, max_ms: 2, mean_ms: 1.5, stdev_ms: 0.1, p50_ms: 1.4, p95_ms: 7 },
      ],
      mean_p50_ms: 1.4,
      mean_p95_ms: 6,
      stdev_p50_ms: 0,
      stdev_p95_ms: 1,
    },
    random_write_4k: null,
  });

  assert.equal(environment.storageBackend, "opfs");
  assert.equal(environment.storageApiMode, "sync_access_handle");

  const metrics = scenarios.storage.metrics;
  assert.equal(metrics.sequential_write_mb_per_s.value, 110); // mean of [100,120]
  assert.equal(metrics.sequential_write_mb_per_s.unit, "MB/s");
  assert.equal(metrics.sequential_write_mb_per_s.better, "higher");
  assert.equal(metrics.sequential_write_mb_per_s.samples.n, 2);
  assert.equal(metrics.sequential_write_mb_per_s.samples.min, 100);
  assert.equal(metrics.sequential_write_mb_per_s.samples.max, 120);

  assert.equal(metrics.random_read_4k_p95_ms.value, 6); // mean of [5,7]
  assert.equal(metrics.random_read_4k_p95_ms.unit, "ms");
  assert.equal(metrics.random_read_4k_p95_ms.better, "lower");
  assert.equal(metrics.random_read_4k_p95_ms.samples.n, 2);
  assert.ok(metrics.random_read_4k_p95_ms.samples.cv > 0);

  assert.ok(!Object.prototype.hasOwnProperty.call(metrics, "random_write_4k_p95_ms"));
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

test("normaliseBenchResult supports aero-gpu-bench schemaVersion=2 (raw+summary) format", () => {
  const { scenarios, environment } = normaliseBenchResult({
    schemaVersion: 2,
    tool: "aero-gpu-bench",
    startedAt: "2025-01-01T00:00:00Z",
    finishedAt: "2025-01-01T00:00:01Z",
    meta: { iterations: 3, nodeVersion: "v20.0.0" },
    environment: { userAgent: "UA", webgpu: false, webgl2: true },
    raw: {
      scenarios: {
        vga_text_scroll: {
          id: "vga_text_scroll",
          name: "VGA text scroll",
          params: {},
          iterations: [
            {
              iteration: 0,
              status: "ok",
              durationMs: 100,
              params: {},
              telemetry: { droppedFrames: 0 },
              derived: { fpsAvg: 60, pipelineCacheHitRate: 0.5 },
            },
            {
              iteration: 1,
              status: "ok",
              durationMs: 100,
              params: {},
              telemetry: { droppedFrames: 1 },
              derived: { fpsAvg: 58, pipelineCacheHitRate: 0.4 },
            },
            {
              iteration: 2,
              status: "ok",
              durationMs: 100,
              params: {},
              telemetry: { droppedFrames: 0 },
              derived: { fpsAvg: 62, pipelineCacheHitRate: 0.6 },
            },
          ],
        },
      },
    },
    summary: {
      scenarios: {
        vga_text_scroll: {
          id: "vga_text_scroll",
          name: "VGA text scroll",
          status: "ok",
          metrics: {},
        },
      },
    },
  });

  assert.equal(environment.userAgent, "UA");
  assert.equal(environment.webgpu, false);
  assert.equal(environment.webgl2, true);
  assert.equal(environment.node, "v20.0.0");
  assert.equal(environment.iterations, 3);

  assert.ok(scenarios["gpu/vga_text_scroll"], "expected normalised gpu/vga_text_scroll scenario");
  assert.equal(scenarios["gpu/vga_text_scroll"].metrics.fps_avg.samples.n, 3);
  assert.equal(scenarios["gpu/vga_text_scroll"].metrics.fps_avg.value, 60); // mean of [60,58,62]
  assert.equal(scenarios["gpu/vga_text_scroll"].metrics.pipeline_cache_hit_rate_pct.value, 50); // mean of [0.5,0.4,0.6]*100
  assert.ok(
    Math.abs(scenarios["gpu/vga_text_scroll"].metrics.dropped_frames.value - 1 / 3) < 1e-9,
    "expected dropped_frames mean to match",
  );
});

test("normaliseBenchResult supports aero-gateway bench results.json format", () => {
  const { scenarios, environment } = normaliseBenchResult({
    meta: {
      mode: "smoke",
      nodeVersion: "v20.0.0",
      platform: "linux",
      arch: "x64",
    },
    tcpProxy: {
      rttMs: {
        n: 100,
        min: 1,
        p50: 2,
        p90: 3,
        p99: 4,
        max: 5,
        mean: 2.5,
        stdev: 1,
        cv: 0.4,
      },
      throughput: {
        bytes: 5 * 1024 * 1024,
        seconds: 1,
        mibPerSecond: 5,
        stats: { n: 3, min: 4, max: 6, mean: 5, stdev: 1, cv: 0.2 },
      },
    },
    doh: {
      qps: 1000,
      qpsStats: { n: 3, min: 900, max: 1100, mean: 1000, stdev: 100, cv: 0.1 },
      latencyMs: { p50: 1, p90: 2, p99: 3, n: 3, min: 1, max: 5, stdev: 1, cv: 0.1 },
      cache: { hits: 96, misses: 4, hitRatio: 0.96 },
      raw: {},
    },
  });

  assert.equal(environment.node, "v20.0.0");
  assert.equal(environment.platform, "linux");
  assert.equal(environment.arch, "x64");
  assert.equal(environment.gatewayMode, "smoke");

  assert.equal(scenarios.gateway.metrics.tcp_rtt_p50_ms.value, 2);
  assert.equal(scenarios.gateway.metrics.tcp_rtt_p50_ms.unit, "ms");
  assert.equal(scenarios.gateway.metrics.tcp_rtt_p50_ms.better, "lower");
  assert.equal(scenarios.gateway.metrics.tcp_rtt_p50_ms.samples.n, 100);
  assert.equal(scenarios.gateway.metrics.tcp_rtt_p50_ms.samples.stdev, 1);
  assert.equal(scenarios.gateway.metrics.tcp_rtt_p50_ms.samples.cv, 0.4);

  assert.equal(scenarios.gateway.metrics.tcp_throughput_mib_s.value, 5);
  assert.equal(scenarios.gateway.metrics.tcp_throughput_mib_s.unit, "MiB/s");
  assert.equal(scenarios.gateway.metrics.tcp_throughput_mib_s.better, "higher");
  assert.equal(scenarios.gateway.metrics.tcp_throughput_mib_s.samples.n, 3);
  assert.equal(scenarios.gateway.metrics.tcp_throughput_mib_s.samples.min, 4);
  assert.equal(scenarios.gateway.metrics.tcp_throughput_mib_s.samples.max, 6);
  assert.equal(scenarios.gateway.metrics.tcp_throughput_mib_s.samples.stdev, 1);
  assert.equal(scenarios.gateway.metrics.tcp_throughput_mib_s.samples.cv, 0.2);

  assert.equal(scenarios.gateway.metrics.doh_qps.value, 1000);
  assert.equal(scenarios.gateway.metrics.doh_qps.unit, "qps");
  assert.equal(scenarios.gateway.metrics.doh_qps.better, "higher");
  assert.equal(scenarios.gateway.metrics.doh_qps.samples.n, 3);
  assert.equal(scenarios.gateway.metrics.doh_qps.samples.stdev, 100);
  assert.equal(scenarios.gateway.metrics.doh_qps.samples.cv, 0.1);

  assert.equal(scenarios.gateway.metrics.doh_latency_p50_ms.value, 1);
  assert.equal(scenarios.gateway.metrics.doh_latency_p50_ms.unit, "ms");
  assert.equal(scenarios.gateway.metrics.doh_latency_p50_ms.better, "lower");
  assert.equal(scenarios.gateway.metrics.doh_latency_p50_ms.samples.n, 3);

  assert.equal(scenarios.gateway.metrics.doh_latency_p90_ms.value, 2);
  assert.equal(scenarios.gateway.metrics.doh_latency_p99_ms.value, 3);

  assert.equal(scenarios.gateway.metrics.doh_cache_hit_ratio_pct.value, 96);
  assert.equal(scenarios.gateway.metrics.doh_cache_hit_ratio_pct.unit, "%");
  assert.equal(scenarios.gateway.metrics.doh_cache_hit_ratio_pct.better, "higher");
  assert.equal(scenarios.gateway.metrics.doh_cache_hit_ratio_pct.samples.n, 100);
});

test("appendHistoryEntry merges multiple inputs for the same entry id", () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "aero-history-"));
  const historyPath = path.join(tmpDir, "history.json");
  const perfPath = path.join(tmpDir, "perf.json");
  const gpuPath = path.join(tmpDir, "gpu.json");
  const gatewayPath = path.join(tmpDir, "gateway.json");
  const storagePath = path.join(tmpDir, "storage.json");

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
  fs.writeFileSync(
    gatewayPath,
    JSON.stringify(
      {
        meta: { mode: "smoke", nodeVersion: "v20.0.0", platform: "linux", arch: "x64", doh: { durationSeconds: 3 } },
        tcpProxy: {
          rttMs: { n: 3, min: 1, p50: 2, p90: 3, p99: 4, max: 5, mean: 2.5, stdev: 1, cv: 0.4 },
          throughput: { bytes: 1024 * 1024, seconds: 1, mibPerSecond: 1, stats: { n: 3, min: 1, max: 1, mean: 1 } },
        },
        doh: { qps: 1000, qpsStats: { n: 3, min: 900, max: 1100, mean: 1000 }, cache: { hits: 1, misses: 0, hitRatio: 1 } },
      },
      null,
      2,
    ),
  );
  fs.writeFileSync(
    storagePath,
    JSON.stringify(
      {
        version: 1,
        run_id: "run-123",
        backend: "opfs",
        api_mode: "sync_access_handle",
        sequential_write: { runs: [{ bytes: 1, duration_ms: 1, mb_per_s: 10 }], mean_mb_per_s: 10, stdev_mb_per_s: 0 },
        sequential_read: { runs: [{ bytes: 1, duration_ms: 1, mb_per_s: 20 }], mean_mb_per_s: 20, stdev_mb_per_s: 0 },
        random_read_4k: {
          runs: [{ ops: 1, block_bytes: 4096, min_ms: 1, max_ms: 1, mean_ms: 1, stdev_ms: 0, p50_ms: 1, p95_ms: 2 }],
          mean_p50_ms: 1,
          mean_p95_ms: 2,
          stdev_p50_ms: 0,
          stdev_p95_ms: 0,
        },
        random_write_4k: null,
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
  appendHistoryEntry({ historyPath, inputPath: gatewayPath, timestamp, commitSha: commit, repository });
  appendHistoryEntry({ historyPath, inputPath: storagePath, timestamp, commitSha: commit, repository });

  const history = JSON.parse(fs.readFileSync(historyPath, "utf8"));
  const entryId = `${timestamp}-${commit}`;
  const entry = history.entries[entryId];
  assert.ok(entry);
  assert.ok(entry.scenarios.browser);
  assert.ok(entry.scenarios["gpu/vga_text_scroll"]);
  assert.ok(entry.scenarios.gateway);
  assert.ok(entry.scenarios.storage);
  assert.equal(entry.environment.node, "v20.0.0");
  assert.equal(entry.environment.userAgent, "UA");
  assert.equal(entry.environment.gatewayMode, "smoke");
  assert.equal(entry.environment.storageBackend, "opfs");
  assert.equal(entry.environment.storageApiMode, "sync_access_handle");
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
