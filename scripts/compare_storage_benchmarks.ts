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

export function compareStorageBenchmarks(params: {
  baseline: any;
  current: any;
  thresholdPct: number;
}): { comparisons: MetricComparison[]; pass: boolean } {
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

  return { comparisons, pass: comparisons.every((c) => !c.regression) };
}

export function renderCompareMarkdown(params: {
  baseline: any;
  current: any;
  thresholdPct: number;
  comparisons: MetricComparison[];
}): string {
  const lines: string[] = [];

  const regressions = params.comparisons.filter((c) => c.regression);
  const pass = regressions.length === 0;

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
  lines.push(`Result: **${pass ? "PASS" : "FAIL"}**`);
  lines.push("");

  if (!pass) {
    lines.push(`Regressions: ${regressions.length}`);
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
  const currentPath = args.current;
  const thresholdPct = args.thresholdPct ? Number(args.thresholdPct) : 15;
  const outDir = args.outDir ?? "storage-perf-results";

  const outputMd = args.outputMd ?? path.join(outDir, "compare.md");
  const outputJson =
    args.outputJson ?? (args.json === "true" ? path.join(outDir, "compare.json") : null);

  if (!baselinePath || !currentPath) {
    throw new Error("Missing required args: --baseline <path> --current <path>");
  }
  if (!Number.isFinite(thresholdPct) || thresholdPct <= 0) {
    throw new Error("--thresholdPct must be a positive number");
  }

  const baseline = JSON.parse(await fs.readFile(baselinePath, "utf8"));
  const current = JSON.parse(await fs.readFile(currentPath, "utf8"));

  const { comparisons, pass } = compareStorageBenchmarks({
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
  });
  await fs.writeFile(outputMd, markdown, "utf8");

  if (outputJson) {
    await fs.mkdir(path.dirname(outputJson), { recursive: true });
    await fs.writeFile(
      outputJson,
      `${JSON.stringify({ thresholdPct, pass, comparisons }, null, 2)}\n`,
      "utf8",
    );
  }

  if (pass) {
    console.log(`OK: no regressions beyond ${thresholdPct}%`);
    return;
  }

  const regressions = comparisons.filter((c) => c.regression);
  console.error(`FAIL: ${regressions.length} regressions beyond ${thresholdPct}%`);
  for (const r of regressions) {
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

