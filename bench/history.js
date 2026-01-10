#!/usr/bin/env node
"use strict";

const fs = require("node:fs");
const path = require("node:path");

const HISTORY_SCHEMA_VERSION = 1;

function computeStats(samples) {
  if (!Array.isArray(samples) || samples.length === 0) {
    throw new Error("Expected non-empty samples array");
  }

  let min = Infinity;
  let max = -Infinity;
  let sum = 0;

  for (const v of samples) {
    if (typeof v !== "number" || !Number.isFinite(v)) {
      throw new Error(`Invalid sample value: ${String(v)}`);
    }
    if (v < min) min = v;
    if (v > max) max = v;
    sum += v;
  }

  const n = samples.length;
  const mean = sum / n;

  let variance = 0;
  if (n > 1) {
    for (const v of samples) variance += (v - mean) ** 2;
    variance /= n - 1;
  }

  const stdev = Math.sqrt(variance);
  const cv = mean === 0 ? 0 : stdev / Math.abs(mean);

  return { n, min, max, mean, stdev, cv };
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

function loadHistory(historyPath) {
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

function normaliseBenchResult(result) {
  if (result === null || typeof result !== "object") {
    throw new Error("Bench result must be an object");
  }
  if (result.schemaVersion !== 1) {
    throw new Error(`Unsupported bench result schemaVersion ${String(result.schemaVersion)}; expected 1`);
  }
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

function appendHistoryEntry({ historyPath, inputPath, timestamp, commitSha, repository, commitUrl }) {
  const history = loadHistory(historyPath);
  const result = normaliseBenchResult(readJson(inputPath));

  const entryId = `${timestamp}-${commitSha}`;
  const url = commitUrl || `https://github.com/${repository}/commit/${commitSha}`;

  history.entries[entryId] = {
    id: entryId,
    timestamp,
    commit: { sha: commitSha, url },
    environment: result.environment,
    scenarios: result.scenarios,
  };

  const sortedEntries = Object.values(history.entries).sort((a, b) => a.timestamp.localeCompare(b.timestamp));
  history.entries = Object.fromEntries(sortedEntries.map((e) => [e.id, e]));
  history.generatedAt = new Date().toISOString();

  writeJsonAtomic(historyPath, history);
}

function formatDelta({ prev, next, better, unit }) {
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

if (require.main === module) {
  main().catch((err) => {
    console.error(err instanceof Error ? err.message : err);
    process.exitCode = 1;
  });
}

module.exports = {
  HISTORY_SCHEMA_VERSION,
  computeStats,
  formatDelta,
  loadHistory,
  normaliseBenchResult,
};
