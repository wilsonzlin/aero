#!/usr/bin/env node
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { RunningStats } from "../packages/aero-stats/src/running-stats.js";
import { formatOneLineError, formatOneLineUtf8 } from "../src/text.js";

export const HISTORY_SCHEMA_VERSION = 1;

export function computeStats(samples) {
  if (!Array.isArray(samples) || samples.length === 0) {
    throw new Error("Expected non-empty samples array");
  }

  const stats = new RunningStats();
  for (const v of samples) {
    if (typeof v !== "number" || !Number.isFinite(v)) {
      const observed =
        typeof v === "string"
          ? (formatOneLineUtf8(v, 64) || "<empty>")
          : v === null
            ? "null"
            : typeof v === "number" || typeof v === "boolean" || typeof v === "bigint"
              ? String(v)
              : `<${typeof v}>`;
      throw new Error(`Invalid sample value (expected finite number): ${observed}`);
    }
    stats.push(v);
  }

  const n = stats.count;
  const mean = stats.mean;
  const stdev = n > 1 ? stats.stdevSample : 0;
  const cv = mean === 0 ? 0 : stdev / Math.abs(mean);

  return { n, min: stats.min, max: stats.max, mean, stdev, cv };
}

function readJson(filePath) {
  return JSON.parse(fs.readFileSync(filePath, "utf8"));
}

function writeJsonAtomic(filePath, data) {
  const dir = path.dirname(filePath);
  const tmpPath = path.join(dir, `.${path.basename(filePath)}.${process.pid}.tmp`);
  fs.writeFileSync(tmpPath, `${JSON.stringify(data, null, 2)}\n`, "utf8");
  fs.renameSync(tmpPath, filePath);
}

export function loadHistory(historyPath) {
  if (!fs.existsSync(historyPath)) {
    return { schemaVersion: HISTORY_SCHEMA_VERSION, entries: {} };
  }

  const history = readJson(historyPath);

  if (history.schemaVersion !== HISTORY_SCHEMA_VERSION) {
    const got =
      typeof history.schemaVersion === "number" || typeof history.schemaVersion === "string"
        ? history.schemaVersion
        : "unknown";
    throw new Error(
      `Unsupported history schemaVersion ${got}; expected ${HISTORY_SCHEMA_VERSION}`,
    );
  }

  if (typeof history.entries !== "object" || history.entries === null || Array.isArray(history.entries)) {
    throw new Error("History entries must be an object keyed by id");
  }

  return history;
}

export function normaliseBenchResult(result) {
  if (result === null || typeof result !== "object") {
    throw new Error("Bench result must be an object");
  }
  if (isGpuBenchReport(result)) {
    return normaliseGpuBenchResult(result);
  }
  if (isAeroGatewayBenchReport(result)) {
    return normaliseAeroGatewayBenchResult(result);
  }
  if (isStorageBenchReport(result)) {
    return normaliseStorageBenchResult(result);
  }
  if (result.schemaVersion === 1) {
    return normaliseLegacyBenchResult(result);
  }
  if (typeof result.scenarioId === "string" && Array.isArray(result.metrics)) {
    return normaliseScenarioRunnerReport(result);
  }
  if (result.meta && Array.isArray(result.benchmarks)) {
    return normalisePerfToolResult(result);
  }
  throw new Error(
    "Unsupported benchmark result format (expected aero-gpu-bench report, aero-gateway bench results.json, storage_bench.json, schemaVersion=1 scenarios, scenario runner report.json, or tools/perf {meta, benchmarks})",
  );
}

function isGpuBenchReport(result) {
  return (
    result &&
    typeof result === "object" &&
    result.tool === "aero-gpu-bench" &&
    result.environment &&
    typeof result.environment === "object" &&
    ((result.scenarios && typeof result.scenarios === "object" && !Array.isArray(result.scenarios)) ||
      (result.raw &&
        typeof result.raw === "object" &&
        result.raw.scenarios &&
        typeof result.raw.scenarios === "object" &&
        !Array.isArray(result.raw.scenarios) &&
        result.summary &&
        typeof result.summary === "object" &&
        result.summary.scenarios &&
        typeof result.summary.scenarios === "object" &&
        !Array.isArray(result.summary.scenarios)))
  );
}

function normaliseGpuBenchResult(result) {
  const scenarios = {};

  /**
   * @param {Record<string, any>} metrics
   * @param {string} name
   * @param {any[]} samples
   * @param {{unit: string, better: "higher" | "lower", scale?: number}=} opts
   */
  function addMetricFromSamples(metrics, name, samples, opts) {
    if (!Array.isArray(samples) || samples.length === 0) return;
    const finite = samples.filter((v) => typeof v === "number" && Number.isFinite(v));
    if (finite.length === 0) return;
    const stats = computeStats(finite);
    const scale = opts?.scale ?? 1;
    const value = stats.mean * scale;
    metrics[name] = {
      value,
      unit: opts?.unit ?? "",
      better: opts?.better ?? "lower",
      samples: {
        n: stats.n,
        min: stats.min * scale,
        max: stats.max * scale,
        stdev: stats.stdev * scale,
        cv: stats.cv,
      },
    };
  }

  const isV2 = result.raw?.scenarios && result.summary?.scenarios;

  if (!isV2) {
    const rawScenarios = result.scenarios;
    if (rawScenarios === null || typeof rawScenarios !== "object" || Array.isArray(rawScenarios)) {
      throw new Error("GPU bench report scenarios must be an object keyed by scenario id");
    }

    for (const [scenarioId, scenario] of Object.entries(rawScenarios)) {
      if (scenario === null || typeof scenario !== "object") {
        throw new Error(`GPU bench scenario ${scenarioId} must be an object`);
      }
      if (scenario.status && scenario.status !== "ok") {
        continue;
      }

      const derived = scenario.derived ?? {};
      const telemetry = scenario.telemetry ?? {};

      /** @type {Record<string, any>} */
      const metrics = {};

      addMetricFromSamples(metrics, "fps_avg", [derived.fpsAvg], { unit: "fps", better: "higher" });
      addMetricFromSamples(metrics, "frame_time_ms_p50", [derived.frameTimeMsP50], { unit: "ms", better: "lower" });
      addMetricFromSamples(metrics, "frame_time_ms_p95", [derived.frameTimeMsP95], { unit: "ms", better: "lower" });
      addMetricFromSamples(metrics, "present_latency_ms_p95", [derived.presentLatencyMsP95], { unit: "ms", better: "lower" });
      addMetricFromSamples(metrics, "shader_translation_ms_mean", [derived.shaderTranslationMsMean], { unit: "ms", better: "lower" });
      addMetricFromSamples(metrics, "shader_compilation_ms_mean", [derived.shaderCompilationMsMean], { unit: "ms", better: "lower" });
      addMetricFromSamples(metrics, "pipeline_cache_hit_rate_pct", [derived.pipelineCacheHitRate], {
        unit: "%",
        better: "higher",
        scale: 100,
      });
      addMetricFromSamples(metrics, "texture_upload_mb_s_avg", [derived.textureUploadMBpsAvg], {
        unit: "MB/s",
        better: "higher",
      });
      addMetricFromSamples(metrics, "dropped_frames", [telemetry.droppedFrames], { unit: "frames", better: "lower" });

      if (Object.keys(metrics).length === 0) {
        continue;
      }

      scenarios[`gpu/${scenarioId}`] = { metrics };
    }
  } else {
    const rawScenarios = result.raw.scenarios;
    const summaryScenarios = result.summary.scenarios;

    for (const [scenarioId, summaryScenario] of Object.entries(summaryScenarios)) {
      if (!summaryScenario || typeof summaryScenario !== "object") continue;
      if (summaryScenario.status !== "ok") continue;

      const rawScenario = rawScenarios?.[scenarioId];
      if (!rawScenario || typeof rawScenario !== "object") continue;

      const iterations = Array.isArray(rawScenario.iterations) ? rawScenario.iterations : [];
      const okIterations = iterations.filter((it) => it && typeof it === "object" && it.status === "ok");

      const samples = {
        fpsAvg: [],
        frameTimeMsP50: [],
        frameTimeMsP95: [],
        presentLatencyMsP95: [],
        shaderTranslationMsMean: [],
        shaderCompilationMsMean: [],
        pipelineCacheHitRate: [],
        textureUploadMBpsAvg: [],
        droppedFrames: [],
      };

      for (const it of okIterations) {
        const derived = it.derived ?? {};
        const telemetry = it.telemetry ?? {};

        if (typeof derived.fpsAvg === "number" && Number.isFinite(derived.fpsAvg)) samples.fpsAvg.push(derived.fpsAvg);
        if (typeof derived.frameTimeMsP50 === "number" && Number.isFinite(derived.frameTimeMsP50)) {
          samples.frameTimeMsP50.push(derived.frameTimeMsP50);
        }
        if (typeof derived.frameTimeMsP95 === "number" && Number.isFinite(derived.frameTimeMsP95)) {
          samples.frameTimeMsP95.push(derived.frameTimeMsP95);
        }
        if (typeof derived.presentLatencyMsP95 === "number" && Number.isFinite(derived.presentLatencyMsP95)) {
          samples.presentLatencyMsP95.push(derived.presentLatencyMsP95);
        }
        if (typeof derived.shaderTranslationMsMean === "number" && Number.isFinite(derived.shaderTranslationMsMean)) {
          samples.shaderTranslationMsMean.push(derived.shaderTranslationMsMean);
        }
        if (typeof derived.shaderCompilationMsMean === "number" && Number.isFinite(derived.shaderCompilationMsMean)) {
          samples.shaderCompilationMsMean.push(derived.shaderCompilationMsMean);
        }
        if (typeof derived.pipelineCacheHitRate === "number" && Number.isFinite(derived.pipelineCacheHitRate)) {
          samples.pipelineCacheHitRate.push(derived.pipelineCacheHitRate);
        }
        if (typeof derived.textureUploadMBpsAvg === "number" && Number.isFinite(derived.textureUploadMBpsAvg)) {
          samples.textureUploadMBpsAvg.push(derived.textureUploadMBpsAvg);
        }
        if (typeof telemetry.droppedFrames === "number" && Number.isFinite(telemetry.droppedFrames)) {
          samples.droppedFrames.push(telemetry.droppedFrames);
        }
      }

      /** @type {Record<string, any>} */
      const metrics = {};

      addMetricFromSamples(metrics, "fps_avg", samples.fpsAvg, { unit: "fps", better: "higher" });
      addMetricFromSamples(metrics, "frame_time_ms_p50", samples.frameTimeMsP50, { unit: "ms", better: "lower" });
      addMetricFromSamples(metrics, "frame_time_ms_p95", samples.frameTimeMsP95, { unit: "ms", better: "lower" });
      addMetricFromSamples(metrics, "present_latency_ms_p95", samples.presentLatencyMsP95, { unit: "ms", better: "lower" });
      addMetricFromSamples(metrics, "shader_translation_ms_mean", samples.shaderTranslationMsMean, {
        unit: "ms",
        better: "lower",
      });
      addMetricFromSamples(metrics, "shader_compilation_ms_mean", samples.shaderCompilationMsMean, {
        unit: "ms",
        better: "lower",
      });
      addMetricFromSamples(metrics, "pipeline_cache_hit_rate_pct", samples.pipelineCacheHitRate, {
        unit: "%",
        better: "higher",
        scale: 100,
      });
      addMetricFromSamples(metrics, "texture_upload_mb_s_avg", samples.textureUploadMBpsAvg, {
        unit: "MB/s",
        better: "higher",
      });
      addMetricFromSamples(metrics, "dropped_frames", samples.droppedFrames, { unit: "frames", better: "lower" });

      if (Object.keys(metrics).length === 0) continue;
      scenarios[`gpu/${scenarioId}`] = { metrics };
    }
  }

  const environment = {};
  if (result.environment && typeof result.environment === "object") {
    if (typeof result.environment.userAgent === "string") environment.userAgent = result.environment.userAgent;
    if (typeof result.environment.webgpu === "boolean") environment.webgpu = result.environment.webgpu;
    if (typeof result.environment.webgl2 === "boolean") environment.webgl2 = result.environment.webgl2;
  }
  if (result.meta && typeof result.meta === "object") {
    if (typeof result.meta.nodeVersion === "string") environment.node = result.meta.nodeVersion;
    if (Number.isFinite(result.meta.iterations)) environment.iterations = result.meta.iterations;
  }

  return { scenarios, environment: Object.keys(environment).length ? environment : undefined };
}

function isAeroGatewayBenchReport(result) {
  if (result && typeof result === "object" && "tool" in result) {
    if (result.tool !== "aero-gateway-bench") return false;
  }
  return (
    result &&
    typeof result === "object" &&
    result.meta &&
    typeof result.meta === "object" &&
    result.tcpProxy &&
    typeof result.tcpProxy === "object" &&
    result.doh &&
    typeof result.doh === "object"
  );
}

function normaliseAeroGatewayBenchResult(result) {
  const metrics = {};

  const tcpProxy = result.tcpProxy ?? {};
  const rtt = tcpProxy.rttMs ?? {};
  const throughput = tcpProxy.throughput ?? {};
  const doh = result.doh ?? {};

  /**
   * @param {number} value
   * @param {{n?: unknown, min?: unknown, max?: unknown, stdev?: unknown, cv?: unknown}} stats
   */
  function samplesFromStats(value, stats) {
    const n = Number.isFinite(stats?.n) && stats.n >= 1 ? Math.trunc(stats.n) : 1;
    const min = typeof stats?.min === "number" && Number.isFinite(stats.min) ? stats.min : value;
    const max = typeof stats?.max === "number" && Number.isFinite(stats.max) ? stats.max : value;
    const stdev =
      typeof stats?.stdev === "number" && Number.isFinite(stats.stdev) && stats.stdev >= 0 ? stats.stdev : 0;
    const cv = typeof stats?.cv === "number" && Number.isFinite(stats.cv) && stats.cv >= 0 ? stats.cv : 0;
    return { n, min, max, stdev, cv };
  }

  /**
   * @param {string} name
   * @param {any} rawValue
   * @param {{unit: string, better: "higher" | "lower", samples?: any}} opts
   */
  function addMetric(name, rawValue, opts) {
    if (rawValue === null || rawValue === undefined) return;
    if (typeof rawValue !== "number" || !Number.isFinite(rawValue)) return;
    metrics[name] = {
      value: rawValue,
      unit: opts.unit,
      better: opts.better,
      samples: samplesFromStats(rawValue, opts.samples ?? {}),
    };
  }

  addMetric("tcp_rtt_p50_ms", rtt.p50, { unit: "ms", better: "lower", samples: rtt });
  addMetric("tcp_rtt_p90_ms", rtt.p90, { unit: "ms", better: "lower", samples: rtt });
  addMetric("tcp_rtt_p99_ms", rtt.p99, { unit: "ms", better: "lower", samples: rtt });
  addMetric("tcp_throughput_mib_s", throughput.mibPerSecond, {
    unit: "MiB/s",
    better: "higher",
    samples: throughput.stats ?? throughput,
  });

  // Prefer explicit DoH variance summaries from the bench report, but fall back to
  // autocannon's raw stats if present (older reports only provided raw.*).
  const dohQpsSamples =
    doh.qpsStats ??
    doh.raw?.requests ??
    // Provide a stable n (for tooltips) when autocannon doesn't provide one.
    (result.meta?.doh && typeof result.meta.doh === "object" ? { n: result.meta.doh.durationSeconds } : undefined);
  addMetric("doh_qps", doh.qps, { unit: "qps", better: "higher", samples: dohQpsSamples });

  addMetric("doh_latency_p50_ms", doh.latencyMs?.p50, { unit: "ms", better: "lower", samples: doh.latencyMs });
  addMetric("doh_latency_p90_ms", doh.latencyMs?.p90, { unit: "ms", better: "lower", samples: doh.latencyMs });
  addMetric("doh_latency_p99_ms", doh.latencyMs?.p99, { unit: "ms", better: "lower", samples: doh.latencyMs });

  const hitRatio = doh.cache?.hitRatio;
  if (typeof hitRatio === "number" && Number.isFinite(hitRatio)) {
    const hits = typeof doh.cache?.hits === "number" && Number.isFinite(doh.cache.hits) ? doh.cache.hits : 0;
    const misses = typeof doh.cache?.misses === "number" && Number.isFinite(doh.cache.misses) ? doh.cache.misses : 0;
    const n = hits + misses > 0 ? hits + misses : 1;
    addMetric("doh_cache_hit_ratio_pct", hitRatio * 100, {
      unit: "%",
      better: "higher",
      samples: { n },
    });
  }

  const scenarios = Object.keys(metrics).length ? { gateway: { metrics } } : {};

  const environment = {};
  if (result.meta && typeof result.meta === "object") {
    if (typeof result.meta.nodeVersion === "string") environment.node = result.meta.nodeVersion;
    if (typeof result.meta.platform === "string") environment.platform = result.meta.platform;
    if (typeof result.meta.arch === "string") environment.arch = result.meta.arch;
    if (typeof result.meta.mode === "string") environment.gatewayMode = result.meta.mode;
  }

  return {
    scenarios,
    environment: Object.keys(environment).length ? environment : undefined,
  };
}

function isStorageBenchReport(result) {
  return (
    result &&
    typeof result === "object" &&
    typeof result.backend === "string" &&
    typeof result.api_mode === "string" &&
    result.sequential_write &&
    typeof result.sequential_write === "object" &&
    result.sequential_read &&
    typeof result.sequential_read === "object" &&
    result.random_read_4k &&
    typeof result.random_read_4k === "object"
  );
}

function normaliseStorageBenchResult(result) {
  /** @type {Record<string, any>} */
  const metrics = {};

  /**
   * @param {Record<string, any>} metrics
   * @param {string} name
   * @param {unknown} summary
   * @param {string} runKey
   * @param {{unit: string, better: "higher" | "lower"}} opts
   */
  function addMetricFromRuns(metrics, name, summary, runKey, opts) {
    if (summary === null || typeof summary !== "object") return;
    const runs = Array.isArray(summary.runs) ? summary.runs : [];
    if (runs.length === 0) return;
    const samples = [];
    for (const run of runs) {
      if (run === null || typeof run !== "object") continue;
      const v = run[runKey];
      if (typeof v !== "number" || !Number.isFinite(v)) continue;
      samples.push(v);
    }
    if (samples.length === 0) return;

    const stats = computeStats(samples);
    metrics[name] = {
      value: stats.mean,
      unit: opts.unit,
      better: opts.better,
      samples: {
        n: stats.n,
        min: stats.min,
        max: stats.max,
        stdev: stats.stdev,
        cv: stats.cv,
      },
    };
  }

  addMetricFromRuns(metrics, "sequential_write_mb_per_s", result.sequential_write, "mb_per_s", {
    unit: "MB/s",
    better: "higher",
  });
  addMetricFromRuns(metrics, "sequential_read_mb_per_s", result.sequential_read, "mb_per_s", {
    unit: "MB/s",
    better: "higher",
  });
  addMetricFromRuns(metrics, "random_read_4k_p95_ms", result.random_read_4k, "p95_ms", {
    unit: "ms",
    better: "lower",
  });
  addMetricFromRuns(metrics, "random_write_4k_p95_ms", result.random_write_4k, "p95_ms", {
    unit: "ms",
    better: "lower",
  });

  const scenarios = Object.keys(metrics).length ? { storage: { metrics } } : {};

  const environment = {};
  if (typeof result.backend === "string") environment.storageBackend = result.backend;
  if (typeof result.api_mode === "string") environment.storageApiMode = result.api_mode;

  return { scenarios, environment: Object.keys(environment).length ? environment : undefined };
}

function normaliseLegacyBenchResult(result) {
  if (result.scenarios === null || typeof result.scenarios !== "object") {
    throw new Error("Bench result scenarios must be an object");
  }

  const scenarios = {};
  for (const [scenarioName, scenario] of Object.entries(result.scenarios)) {
    if (scenario === null || typeof scenario !== "object") {
      throw new Error(`Scenario ${scenarioName} must be an object`);
    }
    if (scenario.metrics === null || typeof scenario.metrics !== "object") {
      throw new Error(`Scenario ${scenarioName} metrics must be an object`);
    }

    const metrics = {};
    for (const [metricName, metric] of Object.entries(scenario.metrics)) {
      if (metric === null || typeof metric !== "object") {
        throw new Error(`Metric ${scenarioName}.${metricName} must be an object`);
      }
      if (!Array.isArray(metric.samples) || metric.samples.length === 0) {
        throw new Error(`Metric ${scenarioName}.${metricName} must provide non-empty samples array`);
      }
      if (typeof metric.unit !== "string" || metric.unit.length === 0) {
        throw new Error(`Metric ${scenarioName}.${metricName} must provide unit`);
      }
      if (metric.better !== "higher" && metric.better !== "lower") {
        throw new Error(`Metric ${scenarioName}.${metricName} must provide better as "higher" or "lower"`);
      }

      const stats = computeStats(metric.samples);
      metrics[metricName] = {
        value: stats.mean,
        unit: metric.unit,
        better: metric.better,
        samples: {
          n: stats.n,
          min: stats.min,
          max: stats.max,
          stdev: stats.stdev,
          cv: stats.cv,
        },
      };
    }

    scenarios[scenarioName] = { metrics };
  }

  const environment = {};
  if (result.meta && typeof result.meta === "object") {
    if (typeof result.meta.node === "string") environment.node = result.meta.node;
    if (typeof result.meta.platform === "string") environment.platform = result.meta.platform;
    if (typeof result.meta.arch === "string") environment.arch = result.meta.arch;
  }

  return { scenarios, environment: Object.keys(environment).length ? environment : undefined };
}

function inferBetter(name, unit) {
  if (unit === "ms" || unit === "s" || unit === "sec" || name.endsWith("_ms") || name.includes("time")) {
    return "lower";
  }
  if (unit === "fps" || name.includes("fps")) return "higher";
  // Generic throughput/rate units.
  if (unit.includes("/s") || unit.includes("per_s") || name.includes("per_s")) return "higher";
  if (unit.includes("ops") || unit.includes("op") || name.includes("ops") || name.includes("ips")) return "higher";
  return "lower";
}

function normaliseScenarioRunnerReport(result) {
  if (result.status && result.status !== "ok") {
    const status =
      typeof result.status === "string" || typeof result.status === "number" || typeof result.status === "boolean" || typeof result.status === "bigint"
        ? String(result.status)
        : "unknown";
    throw new Error(`Scenario runner report status is ${status} (expected ok)`);
  }

  const metrics = {};
  for (const metric of result.metrics) {
    if (metric === null || typeof metric !== "object") {
      throw new Error("Scenario runner metric must be an object");
    }
    if (typeof metric.id !== "string" || metric.id.length === 0) {
      throw new Error("Scenario runner metric.id must be a non-empty string");
    }
    if (typeof metric.unit !== "string" || metric.unit.length === 0) {
      throw new Error(`Scenario runner metric ${metric.id} must provide unit`);
    }
    if (typeof metric.value !== "number" || !Number.isFinite(metric.value)) {
      throw new Error(`Scenario runner metric ${metric.id} must provide finite value`);
    }

    const better = inferBetter(metric.id, metric.unit);
    metrics[metric.id] = {
      value: metric.value,
      unit: metric.unit,
      better,
      samples: {
        n: 1,
        min: metric.value,
        max: metric.value,
        stdev: 0,
        cv: 0,
      },
    };
  }

  return {
    scenarios: {
      [result.scenarioId]: { metrics },
    },
  };
}

function normalisePerfToolResult(result) {
  const metrics = {};

  for (const bench of result.benchmarks) {
    if (bench === null || typeof bench !== "object") {
      throw new Error("tools/perf benchmark must be an object");
    }
    const name = bench.name;
    const unit = bench.unit;
    if (typeof name !== "string" || name.length === 0) {
      throw new Error("tools/perf benchmark.name must be a non-empty string");
    }
    if (typeof unit !== "string" || unit.length === 0) {
      throw new Error(`tools/perf benchmark ${name} must provide a unit`);
    }

    let stats;
    if (bench.stats && typeof bench.stats === "object") {
      const s = bench.stats;
      if (!Number.isFinite(s.n) || !Number.isFinite(s.min) || !Number.isFinite(s.max) || !Number.isFinite(s.mean)) {
        throw new Error(`tools/perf benchmark ${name} has invalid stats`);
      }
      stats = {
        n: s.n,
        min: s.min,
        max: s.max,
        mean: s.mean,
        stdev: Number.isFinite(s.stdev) ? s.stdev : 0,
        cv: Number.isFinite(s.cv) ? s.cv : 0,
      };
    } else if (Array.isArray(bench.samples) && bench.samples.length > 0) {
      stats = computeStats(bench.samples);
    } else {
      throw new Error(`tools/perf benchmark ${name} must provide samples[] or stats`);
    }

    const better = inferBetter(name, unit);
    metrics[name] = {
      value: stats.mean,
      unit,
      better,
      samples: {
        n: stats.n,
        min: stats.min,
        max: stats.max,
        stdev: stats.stdev,
        cv: stats.cv,
      },
    };
  }

  const environment = {};
  if (result.meta && typeof result.meta === "object") {
    if (typeof result.meta.nodeVersion === "string") environment.node = result.meta.nodeVersion;
    if (typeof result.meta.node === "string") environment.node = result.meta.node;

    if (result.meta.os && typeof result.meta.os === "object") {
      if (typeof result.meta.os.platform === "string") environment.platform = result.meta.os.platform;
      if (typeof result.meta.os.arch === "string") environment.arch = result.meta.os.arch;
      if (typeof result.meta.os.release === "string") environment.osRelease = result.meta.os.release;
      if (typeof result.meta.os.cpuModel === "string") environment.cpuModel = result.meta.os.cpuModel;
      if (Number.isFinite(result.meta.os.cpuCount)) environment.cpuCount = result.meta.os.cpuCount;
    }
    if (typeof result.meta.platform === "string") environment.platform = result.meta.platform;
    if (typeof result.meta.arch === "string") environment.arch = result.meta.arch;

    if (typeof result.meta.chromiumVersion === "string") environment.chromiumVersion = result.meta.chromiumVersion;
    if (typeof result.meta.playwrightCoreVersion === "string") {
      environment.playwrightCoreVersion = result.meta.playwrightCoreVersion;
    }
    if (Number.isFinite(result.meta.iterations)) environment.iterations = result.meta.iterations;
    if (typeof result.meta.targetUrl === "string") environment.targetUrl = result.meta.targetUrl;
  }

  return {
    scenarios: {
      browser: { metrics },
    },
    environment: Object.keys(environment).length ? environment : undefined,
  };
}

function mergeEnvironment({ existing, incoming, entryId }) {
  if (!existing) return incoming;
  if (!incoming) return existing;

  const merged = { ...existing };
  for (const [key, value] of Object.entries(incoming)) {
    if (Object.prototype.hasOwnProperty.call(existing, key) && existing[key] !== value) {
      throw new Error(
        `History entry ${entryId} has conflicting environment.${key}: existing=${JSON.stringify(existing[key])} incoming=${JSON.stringify(value)}`,
      );
    }
    merged[key] = value;
  }
  return merged;
}

function mergeScenarios({ existing, incoming, entryId }) {
  const merged = { ...existing };

  for (const [scenarioName, scenario] of Object.entries(incoming)) {
    if (!Object.prototype.hasOwnProperty.call(merged, scenarioName)) {
      merged[scenarioName] = scenario;
      continue;
    }

    const existingScenario = merged[scenarioName];
    const existingMetrics = existingScenario.metrics ?? {};
    const incomingMetrics = scenario.metrics ?? {};

    const nextMetrics = { ...existingMetrics };
    for (const [metricName, metric] of Object.entries(incomingMetrics)) {
      if (Object.prototype.hasOwnProperty.call(nextMetrics, metricName)) {
        throw new Error(
          `History entry ${entryId} has duplicate metric ${scenarioName}.${metricName} across merged inputs`,
        );
      }
      nextMetrics[metricName] = metric;
    }
    merged[scenarioName] = { metrics: nextMetrics };
  }

  return merged;
}

export function appendHistoryEntry({ historyPath, inputPath, timestamp, commitSha, repository, commitUrl }) {
  const history = loadHistory(historyPath);
  const result = normaliseBenchResult(readJson(inputPath));

  const entryId = `${timestamp}-${commitSha}`;
  const url = commitUrl || `https://github.com/${repository}/commit/${commitSha}`;

  const existing = history.entries[entryId];
  if (existing) {
    if (existing.commit?.sha !== commitSha) {
      throw new Error(
        `History entry id collision: ${entryId} already exists for sha=${typeof existing.commit?.sha === "string" ? existing.commit.sha : "unknown"}`,
      );
    }
    if (existing.commit?.url && existing.commit.url !== url) {
      throw new Error(
        `History entry ${entryId} has conflicting commit url: existing=${existing.commit.url} incoming=${url}`,
      );
    }

    history.entries[entryId] = {
      ...existing,
      environment: mergeEnvironment({ existing: existing.environment, incoming: result.environment, entryId }),
      scenarios: mergeScenarios({ existing: existing.scenarios, incoming: result.scenarios, entryId }),
    };
  } else {
    history.entries[entryId] = {
      id: entryId,
      timestamp,
      commit: { sha: commitSha, url },
      environment: result.environment,
      scenarios: result.scenarios,
    };
  }

  const sortedEntries = Object.values(history.entries).sort((a, b) => a.timestamp.localeCompare(b.timestamp));
  history.entries = Object.fromEntries(sortedEntries.map((e) => [e.id, e]));
  history.generatedAt = new Date().toISOString();

  writeJsonAtomic(historyPath, history);
}

export function formatDelta({ prev, next, better, unit }) {
  if (prev === undefined || next === undefined) return { text: "—", className: "neutral" };
  const absolute = next - prev;
  const percent = prev === 0 ? null : absolute / prev;

  let improved;
  if (better === "lower") improved = absolute < 0;
  else improved = absolute > 0;

  const className = improved ? "improvement" : absolute === 0 ? "neutral" : "regression";
  const sign = absolute > 0 ? "+" : "";

  const percentStr =
    percent === null ? "" : ` (${percent > 0 ? "+" : ""}${(percent * 100).toFixed(2)}%)`.replace("+-", "-");

  return {
    text: `${sign}${absolute.toFixed(3)} ${unit}${percentStr}`.replace("+-", "-"),
    className,
  };
}

function renderMarkdown({ historyPath, outPath }) {
  const history = loadHistory(historyPath);
  const entries = Object.values(history.entries).sort((a, b) => a.timestamp.localeCompare(b.timestamp));

  const latest = entries.at(-1);
  const prev = entries.length > 1 ? entries.at(-2) : undefined;

  const lines = [];
  lines.push("# Nightly performance history");
  lines.push("");
  lines.push(`Schema version: ${history.schemaVersion}`);
  lines.push("");
  lines.push(`Total runs: ${entries.length}`);
  lines.push("");

  if (!latest) {
    lines.push("_No benchmark runs recorded yet._");
    lines.push("");
    fs.writeFileSync(outPath, `${lines.join("\n")}\n`, "utf8");
    return;
  }

  lines.push(`Latest run: ${latest.timestamp}`);
  lines.push(`Commit: [\`${latest.commit.sha.slice(0, 7)}\`](${latest.commit.url})`);
  lines.push("");

  lines.push("## Latest metrics");
  lines.push("");
  lines.push("| Scenario | Metric | Value | Δ vs prev | CV |");
  lines.push("| --- | --- | ---: | ---: | ---: |");

  const prevScenarioMetrics = new Map();
  if (prev) {
    for (const [scenarioName, scenario] of Object.entries(prev.scenarios)) {
      for (const [metricName, metric] of Object.entries(scenario.metrics)) {
        prevScenarioMetrics.set(`${scenarioName}.${metricName}`, metric);
      }
    }
  }

  for (const [scenarioName, scenario] of Object.entries(latest.scenarios)) {
    for (const [metricName, metric] of Object.entries(scenario.metrics)) {
      const previous = prevScenarioMetrics.get(`${scenarioName}.${metricName}`);
      const delta = formatDelta({
        prev: previous?.value,
        next: metric.value,
        better: metric.better,
        unit: metric.unit,
      });
      const cvStr = metric.samples.cv === 0 ? "0" : `${(metric.samples.cv * 100).toFixed(2)}%`;
      lines.push(
        `| ${scenarioName} | ${metricName} | ${metric.value.toFixed(3)} ${metric.unit} | ${delta.text} | ${cvStr} |`,
      );
    }
  }

  lines.push("");
  lines.push("## Runs");
  lines.push("");
  lines.push("| Timestamp | Commit |");
  lines.push("| --- | --- |");
  for (const entry of entries.slice().reverse()) {
    lines.push(`| ${entry.timestamp} | [\`${entry.commit.sha.slice(0, 7)}\`](${entry.commit.url}) |`);
  }
  lines.push("");

  fs.writeFileSync(outPath, `${lines.join("\n")}\n`, "utf8");
}

function parseArgs(argv) {
  const [command, ...rest] = argv;
  const options = {};

  for (let i = 0; i < rest.length; i++) {
    const arg = rest[i];
    if (!arg.startsWith("--")) {
      throw new Error(`Unexpected argument: ${arg}`);
    }
    const key = arg.slice(2);
    const value = rest[i + 1];
    if (value === undefined || value.startsWith("--")) {
      throw new Error(`Missing value for --${key}`);
    }
    options[key] = value;
    i++;
  }

  return { command, options };
}

async function main() {
  const { command, options } = parseArgs(process.argv.slice(2));

  if (command === "append") {
    const historyPath = options.history;
    const inputPath = options.input;
    const timestamp = options.timestamp;
    const commitSha = options.commit;
    const repository = options.repository;
    const commitUrl = options["commit-url"];

    if (!historyPath || !inputPath || !timestamp || !commitSha || !repository) {
      throw new Error("append requires --history --input --timestamp --commit --repository");
    }

    appendHistoryEntry({ historyPath, inputPath, timestamp, commitSha, repository, commitUrl });
    return;
  }

  if (command === "render-md") {
    const historyPath = options.history;
    const outPath = options.out;

    if (!historyPath || !outPath) {
      throw new Error("render-md requires --history --out");
    }

    renderMarkdown({ historyPath, outPath });
    return;
  }

  const cmd = typeof command === "string" ? (formatOneLineUtf8(command, 64) || "<empty>") : "unknown";
  throw new Error(`Unknown command: ${cmd}`);
}

if (fileURLToPath(import.meta.url) === path.resolve(process.argv[1] ?? "")) {
  main().catch((err) => {
    console.error(formatOneLineError(err, 512));
    process.exitCode = 1;
  });
}
