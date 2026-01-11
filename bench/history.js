#!/usr/bin/env node
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { RunningStats } from "../packages/aero-stats/src/running-stats.js";

export const HISTORY_SCHEMA_VERSION = 1;

export function computeStats(samples) {
  if (!Array.isArray(samples) || samples.length === 0) {
    throw new Error("Expected non-empty samples array");
  }

  const stats = new RunningStats();
  for (const v of samples) {
    if (typeof v !== "number" || !Number.isFinite(v)) {
      throw new Error(`Invalid sample value: ${String(v)}`);
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
    throw new Error(
      `Unsupported history schemaVersion ${String(history.schemaVersion)}; expected ${HISTORY_SCHEMA_VERSION}`,
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
    "Unsupported benchmark result format (expected aero-gpu-bench report, schemaVersion=1 scenarios, scenario runner report.json, or tools/perf {meta, benchmarks})",
  );
}

function isGpuBenchReport(result) {
  return (
    result &&
    typeof result === "object" &&
    result.tool === "aero-gpu-bench" &&
    result.environment &&
    typeof result.environment === "object" &&
    result.scenarios &&
    typeof result.scenarios === "object" &&
    !Array.isArray(result.scenarios)
  );
}

function normaliseGpuBenchResult(result) {
  const rawScenarios = result.scenarios;
  if (rawScenarios === null || typeof rawScenarios !== "object" || Array.isArray(rawScenarios)) {
    throw new Error("GPU bench report scenarios must be an object keyed by scenario id");
  }

  const scenarios = {};

  /**
   * @param {Record<string, any>} metrics
   * @param {string} name
   * @param {any} rawValue
   * @param {{unit: string, better: "higher" | "lower", scale?: number}=} opts
   */
  function addMetric(metrics, name, rawValue, opts) {
    if (rawValue === null || rawValue === undefined) return;
    if (typeof rawValue !== "number" || !Number.isFinite(rawValue)) return;
    const value = opts?.scale ? rawValue * opts.scale : rawValue;
    if (!Number.isFinite(value)) return;
    metrics[name] = {
      value,
      unit: opts?.unit ?? "",
      better: opts?.better ?? "lower",
      samples: {
        n: 1,
        min: value,
        max: value,
        stdev: 0,
        cv: 0,
      },
    };
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

    addMetric(metrics, "fps_avg", derived.fpsAvg, { unit: "fps", better: "higher" });
    addMetric(metrics, "frame_time_ms_p50", derived.frameTimeMsP50, { unit: "ms", better: "lower" });
    addMetric(metrics, "frame_time_ms_p95", derived.frameTimeMsP95, { unit: "ms", better: "lower" });
    addMetric(metrics, "present_latency_ms_p95", derived.presentLatencyMsP95, { unit: "ms", better: "lower" });
    addMetric(metrics, "shader_translation_ms_mean", derived.shaderTranslationMsMean, { unit: "ms", better: "lower" });
    addMetric(metrics, "shader_compilation_ms_mean", derived.shaderCompilationMsMean, { unit: "ms", better: "lower" });
    addMetric(metrics, "pipeline_cache_hit_rate_pct", derived.pipelineCacheHitRate, {
      unit: "%",
      better: "higher",
      scale: 100,
    });
    addMetric(metrics, "texture_upload_mb_s_avg", derived.textureUploadMBpsAvg, { unit: "MB/s", better: "higher" });
    addMetric(metrics, "dropped_frames", telemetry.droppedFrames, { unit: "frames", better: "lower" });

    if (Object.keys(metrics).length === 0) {
      continue;
    }

    scenarios[`gpu/${scenarioId}`] = { metrics };
  }

  const environment = {};
  if (result.environment && typeof result.environment === "object") {
    if (typeof result.environment.userAgent === "string") environment.userAgent = result.environment.userAgent;
    if (typeof result.environment.webgpu === "boolean") environment.webgpu = result.environment.webgpu;
    if (typeof result.environment.webgl2 === "boolean") environment.webgl2 = result.environment.webgl2;
  }

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
  if (unit.includes("ops") || unit.includes("op") || name.includes("ops") || name.includes("ips")) return "higher";
  return "lower";
}

function normaliseScenarioRunnerReport(result) {
  if (result.status && result.status !== "ok") {
    throw new Error(`Scenario runner report status is ${String(result.status)} (expected ok)`);
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
        `History entry id collision: ${entryId} already exists for sha=${String(existing.commit?.sha)}`,
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

  throw new Error(`Unknown command: ${String(command)}`);
}

if (fileURLToPath(import.meta.url) === path.resolve(process.argv[1] ?? "")) {
  main().catch((err) => {
    console.error(err instanceof Error ? err.message : err);
    process.exitCode = 1;
  });
}
