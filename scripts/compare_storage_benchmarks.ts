/**
 * Compare two `storage_bench.json` reports and fail on regressions.
 *
 * Usage:
 *   node --experimental-strip-types scripts/compare_storage_benchmarks.ts \
 *     --baseline path/to/storage_bench.json \
 *     --current path/to/storage_bench.json \
 *     --thresholdPct 15
 *
 * Output:
 *   - storage-perf-results/compare.md
 *   - storage-perf-results/compare.json (optional; pass --json or --outputJson)
 *
 * The script exits with code 1 if any checked metric regresses by more than the
 * configured threshold percentage.
 */

import fs from "node:fs/promises";
import path from "node:path";

export type BetterDirection = "higher" | "lower";

export interface MetricComparison {
  metric: string;
  better: BetterDirection;
  unit: string;
  baseline: number | null;
  current: number | null;
  deltaPct: number | null;
  regression: boolean;
  note?: string;
}

export interface MetadataComparison {
  field: string;
  baseline: string | null;
  current: string | null;
  regression: boolean;
  note?: string;
}

function parseArgs(argv: string[]): Record<string, string> {
  const out: Record<string, string> = {};
  for (let i = 0; i < argv.length; i += 1) {
    const a = argv[i];
    if (!a.startsWith("--")) continue;
    const k = a.slice(2);
    const v = argv[i + 1];
    if (v && !v.startsWith("--")) {
      out[k] = v;
      i += 1;
    } else {
      out[k] = "true";
    }
  }
  return out;
}

export function isFiniteNumber(n: unknown): n is number {
  return typeof n === "number" && Number.isFinite(n);
}

export function compareMetric(params: {
  baseline: number;
  current: number;
  better: BetterDirection;
  threshold: number;
}): { deltaPct: number; regression: boolean } {
  const deltaPct = (params.current - params.baseline) / params.baseline;
  const regression =
    params.better === "lower" ? deltaPct > params.threshold : deltaPct < -params.threshold;
  return { deltaPct, regression };
}

function fmtNumber(value: number, digits = 2): string {
  return value.toFixed(digits);
}

function fmtPct(value: number): string {
  return `${(value * 100).toFixed(2)}%`;
}

function fmtSignedPct(value: number): string {
  const sign = value >= 0 ? "+" : "";
  return `${sign}${fmtPct(value)}`.replace("+-", "-");
}

function getNumber(obj: any, getter: (v: any) => unknown): number | null {
  const value = getter(obj);
  return isFiniteNumber(value) ? value : null;
}

function getString(obj: any, getter: (v: any) => unknown): string | null {
  const value = getter(obj);
  return typeof value === "string" && value.length > 0 ? value : null;
}

function formatConfigSummary(cfg: any): string | null {
  if (!cfg || typeof cfg !== "object" || Array.isArray(cfg)) return null;

  const keys = [
    "seq_total_mb",
    "seq_chunk_mb",
    "seq_runs",
    "warmup_mb",
    "random_ops",
    "random_runs",
    "random_space_mb",
    "random_seed",
    "include_random_write",
  ];

  const parts: string[] = [];
  for (const key of keys) {
    const value = (cfg as any)[key];
    parts.push(`${key}=${value === undefined ? "unset" : String(value)}`);
  }
  return parts.join(" ");
}

function compareOrderedField(params: {
  field: string;
  baseline: string | null;
  current: string | null;
  ranks: Record<string, number>;
}): MetadataComparison {
  if (!params.baseline || !params.current) {
    return {
      field: params.field,
      baseline: params.baseline,
      current: params.current,
      regression: true,
      note: "missing/invalid baseline or current value",
    };
  }

  if (params.baseline === params.current) {
    return {
      field: params.field,
      baseline: params.baseline,
      current: params.current,
      regression: false,
    };
  }

  const baseRank = params.ranks[params.baseline];
  const curRank = params.ranks[params.current];
  if (Number.isFinite(baseRank) && Number.isFinite(curRank)) {
    const regression = curRank < baseRank;
    return {
      field: params.field,
      baseline: params.baseline,
      current: params.current,
      regression,
      note: regression ? "capability regressed" : "capability improved/changed",
    };
  }

  // If we don't understand one of the values, treat it as a regression to avoid silently
  // passing on an apples-to-oranges comparison.
  return {
    field: params.field,
    baseline: params.baseline,
    current: params.current,
    regression: true,
    note: "unknown baseline/current value",
  };
}

export function compareStorageBenchmarks(params: {
  baseline: any;
  current: any;
  thresholdPct: number;
}): { comparisons: MetricComparison[]; metadataComparisons: MetadataComparison[]; pass: boolean } {
  const threshold = params.thresholdPct / 100;

  const metrics: Array<{
    metric: string;
    unit: string;
    better: BetterDirection;
    optional?: boolean;
    get: (r: any) => number | null;
  }> = [
    {
      metric: "sequential_write.mean_mb_per_s",
      unit: "MB/s",
      better: "higher",
      get: (r) => getNumber(r, (v) => v?.sequential_write?.mean_mb_per_s),
    },
    {
      metric: "sequential_read.mean_mb_per_s",
      unit: "MB/s",
      better: "higher",
      get: (r) => getNumber(r, (v) => v?.sequential_read?.mean_mb_per_s),
    },
    {
      metric: "random_read_4k.mean_p50_ms",
      unit: "ms",
      better: "lower",
      get: (r) => getNumber(r, (v) => v?.random_read_4k?.mean_p50_ms),
    },
    {
      metric: "random_read_4k.mean_p95_ms",
      unit: "ms",
      better: "lower",
      get: (r) => getNumber(r, (v) => v?.random_read_4k?.mean_p95_ms),
    },
    {
      metric: "random_write_4k.mean_p95_ms",
      unit: "ms",
      better: "lower",
      optional: true,
      get: (r) => getNumber(r, (v) => v?.random_write_4k?.mean_p95_ms),
    },
  ];

  const comparisons: MetricComparison[] = [];

  for (const m of metrics) {
    const baselineVal = m.get(params.baseline);
    const currentVal = m.get(params.current);

    if (m.optional && baselineVal === null && currentVal === null) {
      continue;
    }

    if (!isFiniteNumber(baselineVal) || !isFiniteNumber(currentVal) || baselineVal === 0) {
      comparisons.push({
        metric: m.metric,
        better: m.better,
        unit: m.unit,
        baseline: baselineVal,
        current: currentVal,
        deltaPct: null,
        regression: true,
        note: "missing/invalid baseline or current value",
      });
      continue;
    }

    const { deltaPct, regression } = compareMetric({
      baseline: baselineVal,
      current: currentVal,
      better: m.better,
      threshold,
    });

    comparisons.push({
      metric: m.metric,
      better: m.better,
      unit: m.unit,
      baseline: baselineVal,
      current: currentVal,
      deltaPct,
      regression,
    });
  }

  const metadataComparisons: MetadataComparison[] = [];
  const baselineBackend = getString(params.baseline, (v) => v?.backend);
  const currentBackend = getString(params.current, (v) => v?.backend);
  metadataComparisons.push(
    compareOrderedField({
      field: "backend",
      baseline: baselineBackend,
      current: currentBackend,
      ranks: { indexeddb: 1, opfs: 2 },
    }),
  );

  const baselineApiMode = getString(params.baseline, (v) => v?.api_mode);
  const currentApiMode = getString(params.current, (v) => v?.api_mode);
  // Only gate api_mode if we stayed on OPFS; IndexedDB is always async.
  if (baselineBackend === "opfs" && currentBackend === "opfs") {
    metadataComparisons.push(
      compareOrderedField({
        field: "api_mode",
        baseline: baselineApiMode,
        current: currentApiMode,
        ranks: { async: 1, sync_access_handle: 2 },
      }),
    );
  }

  const baselineConfig = params.baseline?.config;
  const currentConfig = params.current?.config;
  if (baselineConfig && typeof baselineConfig === "object" && currentConfig && typeof currentConfig === "object") {
    const keys = [
      "seq_total_mb",
      "seq_chunk_mb",
      "seq_runs",
      "warmup_mb",
      "random_ops",
      "random_runs",
      "random_space_mb",
      "random_seed",
      "include_random_write",
    ];

    const diffs: string[] = [];
    for (const key of keys) {
      const a = (baselineConfig as any)[key];
      const b = (currentConfig as any)[key];
      if (a !== b) diffs.push(`${key}: ${a === undefined ? "unset" : a} -> ${b === undefined ? "unset" : b}`);
    }

    if (diffs.length > 0) {
      metadataComparisons.push({
        field: "config",
        baseline: formatConfigSummary(baselineConfig),
        current: formatConfigSummary(currentConfig),
        regression: true,
        note: diffs.join(", "),
      });
    }
  }

  const pass =
    comparisons.every((c) => !c.regression) && metadataComparisons.every((c) => !c.regression);

  return { comparisons, metadataComparisons, pass };
}

export function renderCompareMarkdown(params: {
  baseline: any;
  current: any;
  thresholdPct: number;
  comparisons: MetricComparison[];
  metadataComparisons: MetadataComparison[];
}): string {
  const lines: string[] = [];

  const metricRegressions = params.comparisons.filter((c) => c.regression);
  const metadataRegressions = params.metadataComparisons.filter((c) => c.regression);
  const regressions = metricRegressions.length + metadataRegressions.length;
  const pass = regressions === 0;

  lines.push("# Storage perf comparison");
  lines.push("");
  lines.push(`Threshold: ${params.thresholdPct}%`);
  lines.push("");
  lines.push(
    `Baseline: backend=\`${params.baseline?.backend ?? "unknown"}\` api_mode=\`${params.baseline?.api_mode ?? "unknown"}\``,
  );
  lines.push(
    `Current: backend=\`${params.current?.backend ?? "unknown"}\` api_mode=\`${params.current?.api_mode ?? "unknown"}\``,
  );
  lines.push("");

  if (params.metadataComparisons.length > 0) {
    lines.push("## Context");
    lines.push("");
    lines.push("| Field | Baseline | Current | Status | Note |");
    lines.push("| --- | --- | --- | --- | --- |");
    for (const c of params.metadataComparisons) {
      const status = c.regression ? "FAIL" : c.baseline === c.current ? "OK" : "WARN";
      lines.push(
        `| ${c.field} | ${c.baseline ?? "n/a"} | ${c.current ?? "n/a"} | ${status} | ${c.note ?? ""} |`,
      );
    }
    lines.push("");
  }

  const baselineWarnings = Array.isArray(params.baseline?.warnings) ? params.baseline.warnings : [];
  const currentWarnings = Array.isArray(params.current?.warnings) ? params.current.warnings : [];
  if (baselineWarnings.length > 0 || currentWarnings.length > 0) {
    lines.push("## Warnings");
    lines.push("");
    if (baselineWarnings.length > 0) {
      lines.push("Baseline warnings:");
      for (const w of baselineWarnings) {
        lines.push(`- ${w}`);
      }
      lines.push("");
    }
    if (currentWarnings.length > 0) {
      lines.push("Current warnings:");
      for (const w of currentWarnings) {
        lines.push(`- ${w}`);
      }
      lines.push("");
    }
  }

  lines.push(`Result: **${pass ? "PASS" : "FAIL"}**`);
  lines.push("");

  if (!pass) {
    lines.push(`Regressions: ${regressions}`);
    lines.push("");
  }

  lines.push("| Metric | Better | Baseline | Current | Δ | Status |");
  lines.push("| --- | --- | ---: | ---: | ---: | --- |");

  for (const c of params.comparisons) {
    const baselineStr =
      c.baseline === null ? "n/a" : `${fmtNumber(c.baseline)} ${c.unit}`.trim();
    const currentStr = c.current === null ? "n/a" : `${fmtNumber(c.current)} ${c.unit}`.trim();
    const deltaStr = c.deltaPct === null ? "n/a" : fmtSignedPct(c.deltaPct);
    const statusStr = c.regression ? `FAIL${c.note ? ` (${c.note})` : ""}` : "PASS";
    lines.push(`| ${c.metric} | ${c.better} | ${baselineStr} | ${currentStr} | ${deltaStr} | ${statusStr} |`);
  }

  lines.push("");
  return `${lines.join("\n")}\n`;
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  const baselinePath = args.baseline;
  const currentPath = args.current ?? args.candidate;
  const thresholdPct = args.thresholdPct
    ? Number(args.thresholdPct)
    : process.env.STORAGE_PERF_REGRESSION_THRESHOLD_PCT
      ? Number(process.env.STORAGE_PERF_REGRESSION_THRESHOLD_PCT)
      : 15;
  const outDir = args.outDir ?? args["out-dir"] ?? "storage-perf-results";

  const outputMd = args.outputMd ?? args["output-md"] ?? path.join(outDir, "compare.md");
  const outputJson =
    args.outputJson ??
    args["output-json"] ??
    (args.json === "true" ? path.join(outDir, "compare.json") : null);

  if (!baselinePath || !currentPath) {
    throw new Error("Missing required args: --baseline <path> --current <path> (or --candidate <path>)");
  }
  if (!Number.isFinite(thresholdPct) || thresholdPct <= 0) {
    throw new Error("--thresholdPct must be a positive number");
  }

  const baseline = JSON.parse(await fs.readFile(baselinePath, "utf8"));
  const current = JSON.parse(await fs.readFile(currentPath, "utf8"));

  const { comparisons, metadataComparisons, pass } = compareStorageBenchmarks({
    baseline,
    current,
    thresholdPct,
  });

  await fs.mkdir(path.dirname(outputMd), { recursive: true });

  const markdown = renderCompareMarkdown({
    baseline,
    current,
    thresholdPct,
    comparisons,
    metadataComparisons,
  });
  await fs.writeFile(outputMd, markdown, "utf8");

  if (outputJson) {
    await fs.mkdir(path.dirname(outputJson), { recursive: true });
    await fs.writeFile(
      outputJson,
      `${JSON.stringify({ thresholdPct, pass, metadataComparisons, comparisons }, null, 2)}\n`,
      "utf8",
    );
  }

  if (pass) {
    console.log(`OK: no regressions beyond ${thresholdPct}%`);
    return;
  }

  const metricRegressions = comparisons.filter((c) => c.regression);
  const metaRegressions = metadataComparisons.filter((c) => c.regression);
  const total = metricRegressions.length + metaRegressions.length;
  console.error(`FAIL: ${total} regressions beyond ${thresholdPct}%`);
  for (const r of metaRegressions) {
    console.error(
      `- ${r.field}: baseline=${r.baseline ?? "n/a"} current=${r.current ?? "n/a"} (${r.note ?? "regressed"})`,
    );
  }
  for (const r of metricRegressions) {
    const base = r.baseline ?? "n/a";
    const cur = r.current ?? "n/a";
    const delta = r.deltaPct === null ? "n/a" : fmtSignedPct(r.deltaPct);
    console.error(`- ${r.metric}: baseline=${base} current=${cur} (${delta})`);
  }
  process.exitCode = 1;
}

if (import.meta.url === `file://${process.argv[1]}`) {
  main().catch((err) => {
    console.error(err?.stack ?? String(err));
    process.exitCode = 1;
  });
}
