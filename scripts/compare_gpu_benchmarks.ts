/**
 * Compare two GPU benchmark reports and emit a Markdown + JSON summary.
 *
 * Usage:
 *   node --experimental-strip-types scripts/compare_gpu_benchmarks.ts \
 *     --baseline path/to/baseline.json \
 *     --current path/to/current.json \
 *     --thresholdPct 5 \
 *     --cvThreshold 0.5
 *
 * Outputs (written to `--outDir` or next to `--current` by default):
 * - compare.md
 * - summary.json
 *
 * Exit codes:
 * - 0: pass
 * - 1: regression
 * - 2: unstable (extreme variance)
 */

import fs from "node:fs/promises";
import path from "node:path";
import process from "node:process";

function usage(exitCode) {
  const msg = `
Usage:
  node --experimental-strip-types scripts/compare_gpu_benchmarks.ts \\
    --baseline <path> \\
    --current <path>

Options:
  --baseline <path>      Baseline GPU report (required)
  --current <path>       Current GPU report (required)
  --thresholdPct <n>     Fail if any metric regresses by >= n percent (default: 5)
  --cvThreshold <n>      Fail as unstable if any metric has CV >= n (default: 0.5)
  --outDir <dir>         Output directory (default: dirname(--current))
  --help                 Show this help
`;
  console.log(msg.trim());
  process.exit(exitCode);
}

/**
 * @param {any} n
 */
function isFiniteNumber(n) {
  return typeof n === "number" && Number.isFinite(n);
}

/**
 * @param {any} s
 */
function mdEscape(s) {
  return String(s).replaceAll("|", "\\|");
}

/**
 * @param {number|null} v
 * @param {string} unit
 */
function fmtValue(v, unit) {
  if (!isFiniteNumber(v)) return "n/a";
  if (unit === "ratio") return `${(v * 100).toFixed(1)}%`;
  if (unit === "fps") return `${v.toFixed(2)} fps`;
  if (unit === "mbps") return `${v.toFixed(2)} MB/s`;
  if (unit === "ms") return `${v.toFixed(2)} ms`;
  return v.toFixed(3);
}

/**
 * @param {number|null} pct
 */
function fmtSignedPct(pct) {
  if (!isFiniteNumber(pct)) return "n/a";
  const sign = pct >= 0 ? "+" : "";
  return `${sign}${(pct * 100).toFixed(2)}%`;
}

/**
 * @param {string} metric
 */
function metricLabel(metric) {
  return metric;
}

const METRICS = [
  { key: "frameTimeMsP95", better: "lower", unit: "ms" },
  { key: "shaderTranslationMsMean", better: "lower", unit: "ms" },
  { key: "shaderCompilationMsMean", better: "lower", unit: "ms" },
  { key: "presentLatencyMsP95", better: "lower", unit: "ms" },
  { key: "fpsAvg", better: "higher", unit: "fps" },
  { key: "frameTimeMsP50", better: "lower", unit: "ms" },
  { key: "textureUploadMBpsAvg", better: "higher", unit: "mbps" },
  { key: "pipelineCacheHitRate", better: "higher", unit: "ratio" },
];

/**
 * @param {string[]} argv
 */
function parseArgs(argv) {
  const out = {
    baseline: undefined,
    current: undefined,
    thresholdPct: 5,
    cvThreshold: 0.5,
    outDir: undefined,
  };

  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    switch (arg) {
      case "--baseline":
        out.baseline = argv[++i];
        break;
      case "--current":
        out.current = argv[++i];
        break;
      case "--thresholdPct":
        out.thresholdPct = Number.parseFloat(argv[++i]);
        break;
      case "--cvThreshold":
        out.cvThreshold = Number.parseFloat(argv[++i]);
        break;
      case "--outDir":
      case "--out-dir":
        out.outDir = argv[++i];
        break;
      case "--help":
        usage(0);
        break;
      default:
        if (arg.startsWith("-")) {
          console.error(`Unknown option: ${arg}`);
          usage(1);
        }
        break;
    }
  }

  if (!out.baseline || !out.current) {
    console.error("Missing required args: --baseline <path> --current <path>");
    usage(1);
  }
  if (!Number.isFinite(out.thresholdPct) || out.thresholdPct <= 0) {
    console.error("--thresholdPct must be a positive number");
    usage(1);
  }
  if (!Number.isFinite(out.cvThreshold) || out.cvThreshold <= 0) {
    console.error("--cvThreshold must be a positive number");
    usage(1);
  }

  return out;
}

/**
 * @param {{baseline:number, current:number, better:"lower"|"higher", thresholdPct:number}} cfg
 */
function compareMetric(cfg) {
  const deltaPct = (cfg.current - cfg.baseline) / cfg.baseline;
  const threshold = cfg.thresholdPct / 100;
  const regression = cfg.better === "lower" ? deltaPct >= threshold : deltaPct <= -threshold;
  return { deltaPct, regression };
}

/**
 * @typedef {{
 *   scenarioId: string,
 *   scenarioName: string,
 *   metric: string,
 *   better: "lower"|"higher",
 *   unit: string,
 *   baselineMedian: number|null,
 *   currentMedian: number|null,
 *   deltaPct: number|null,
 *   baselineCv: number|null,
 *   currentCv: number|null,
 *   regression: boolean,
 *   unstable: boolean,
 * }} CompareRow
 */

/**
 * Compare two GPU benchmark reports.
 *
 * This is kept pure so it can be unit-tested without spawning a subprocess.
 *
 * @param {{
 *   baseline: any,
 *   current: any,
 *   thresholdPct: number,
 *   cvThreshold: number,
 * }} opts
 * @returns {{
 *   status: "pass"|"fail"|"unstable",
 *   hasRegression: boolean,
 *   isUnstable: boolean,
 *   rows: CompareRow[],
 * }}
 */
export function compareGpuBenchmarks(opts) {
  const baseScenarios = opts.baseline?.summary?.scenarios ?? {};
  const curScenarios = opts.current?.summary?.scenarios ?? {};

  const scenarioIds = new Set([...Object.keys(baseScenarios), ...Object.keys(curScenarios)]);

  /** @type {CompareRow[]} */
  const rows = [];
  let hasRegression = false;
  let isUnstable = false;

  for (const scenarioId of Array.from(scenarioIds).sort()) {
    const b = baseScenarios[scenarioId];
    const c = curScenarios[scenarioId];

    const scenarioName = c?.name ?? b?.name ?? scenarioId;
    const ok = b?.status === "ok" && c?.status === "ok";

    for (const metricCfg of METRICS) {
      const bStats = b?.metrics?.[metricCfg.key] ?? null;
      const cStats = c?.metrics?.[metricCfg.key] ?? null;
      const baselineMedian = isFiniteNumber(bStats?.median) ? bStats.median : isFiniteNumber(bStats?.p50) ? bStats.p50 : null;
      const currentMedian = isFiniteNumber(cStats?.median) ? cStats.median : isFiniteNumber(cStats?.p50) ? cStats.p50 : null;

      const baselineCv = isFiniteNumber(bStats?.cv) ? bStats.cv : null;
      const currentCv = isFiniteNumber(cStats?.cv) ? cStats.cv : null;

      const unstable =
        (baselineCv != null && baselineCv >= opts.cvThreshold) ||
        (currentCv != null && currentCv >= opts.cvThreshold) ||
        !ok;
      if (unstable) isUnstable = true;

      let deltaPct = null;
      let regression = false;
      if (baselineMedian != null && currentMedian != null && baselineMedian !== 0) {
        const cmp = compareMetric({
          baseline: baselineMedian,
          current: currentMedian,
          better: metricCfg.better,
          thresholdPct: opts.thresholdPct,
        });
        deltaPct = cmp.deltaPct;
        regression = cmp.regression;
        if (regression) hasRegression = true;
      }

      rows.push({
        scenarioId,
        scenarioName,
        metric: metricCfg.key,
        better: metricCfg.better,
        unit: metricCfg.unit,
        baselineMedian,
        currentMedian,
        deltaPct,
        baselineCv,
        currentCv,
        regression,
        unstable,
      });
    }
  }

  const status = isUnstable ? "unstable" : hasRegression ? "fail" : "pass";
  return { status, hasRegression, isUnstable, rows };
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  const baselinePath = args.baseline;
  const currentPath = args.current;
  const outDir = path.resolve(process.cwd(), args.outDir ?? path.dirname(currentPath));

  await fs.mkdir(outDir, { recursive: true });

  const baseline = JSON.parse(await fs.readFile(baselinePath, "utf8"));
  const current = JSON.parse(await fs.readFile(currentPath, "utf8"));

  const result = compareGpuBenchmarks({
    baseline,
    current,
    thresholdPct: args.thresholdPct,
    cvThreshold: args.cvThreshold,
  });

  const rows = result.rows;
  const regressions = rows.filter((r) => r.regression).sort((a, b) => (b.deltaPct ?? 0) - (a.deltaPct ?? 0));
  const unstableRows = rows.filter((r) => r.unstable);

  const reportLines = [];
  reportLines.push("# GPU perf comparison");
  reportLines.push("");
  reportLines.push(`- Baseline: \`${baseline.meta?.gitSha ?? "unknown"}\``);
  reportLines.push(`- Current: \`${current.meta?.gitSha ?? "unknown"}\``);
  reportLines.push(`- Threshold: ${args.thresholdPct}% regression (median-of-N)`);
  reportLines.push(`- CV threshold: ${args.cvThreshold}`);
  reportLines.push(
    `- Iterations: baseline=${baseline.meta?.iterations ?? "?"} current=${current.meta?.iterations ?? "?"}`,
  );
  reportLines.push("");
  reportLines.push("| Scenario | Metric | Baseline (median) | Current (median) | Δ% | Baseline CV | Current CV |");
  reportLines.push("| --- | --- | ---: | ---: | ---: | ---: | ---: |");
  for (const r of rows) {
    reportLines.push(
      `| ${mdEscape(r.scenarioName)} | ${mdEscape(metricLabel(r.metric))} | ${fmtValue(r.baselineMedian, r.unit)} | ${fmtValue(r.currentMedian, r.unit)} | ${fmtSignedPct(r.deltaPct)} | ${
        r.baselineCv != null ? r.baselineCv.toFixed(2) : "n/a"
      } | ${r.currentCv != null ? r.currentCv.toFixed(2) : "n/a"} |`,
    );
  }

  reportLines.push("");
  if (result.status === "unstable") {
    reportLines.push("Result: **Unstable** (extreme variance or failed scenarios; see details below)");
  } else if (result.status === "fail") {
    reportLines.push("Result: **Regression detected**");
  } else {
    reportLines.push("Result: **No significant regressions**");
  }

  if (regressions.length > 0) {
    reportLines.push("");
    reportLines.push("## Regressions");
    reportLines.push("");
    for (const r of regressions.slice(0, 10)) {
      reportLines.push(
        `- **${mdEscape(r.scenarioName)}.${mdEscape(r.metric)}**: ${fmtSignedPct(r.deltaPct)} (baseline ${fmtValue(r.baselineMedian, r.unit)} → current ${fmtValue(r.currentMedian, r.unit)})`,
      );
    }
  }

  if (unstableRows.length > 0) {
    reportLines.push("");
    reportLines.push("## Unstable metrics / scenarios");
    reportLines.push("");
    reportLines.push(`- Any metric with CV >= ${args.cvThreshold} (baseline or current) is considered unstable.`);
    reportLines.push("- Any scenario with status != ok is considered unstable (missing data).");
    reportLines.push("");
    for (const r of unstableRows.slice(0, 20)) {
      reportLines.push(
        `- ${mdEscape(r.scenarioName)}.${mdEscape(r.metric)}: baseline CV=${r.baselineCv == null ? "n/a" : r.baselineCv.toFixed(2)}, current CV=${r.currentCv == null ? "n/a" : r.currentCv.toFixed(2)}`,
      );
    }
  }

  const summary = {
    status: result.status,
    thresholdPct: args.thresholdPct,
    cvThreshold: args.cvThreshold,
    baseline: { gitSha: baseline.meta?.gitSha, path: baselinePath, iterations: baseline.meta?.iterations },
    current: { gitSha: current.meta?.gitSha, path: currentPath, iterations: current.meta?.iterations },
    rows: rows.map((r) => ({
      scenarioId: r.scenarioId,
      metric: r.metric,
      better: r.better,
      baselineMedian: r.baselineMedian,
      currentMedian: r.currentMedian,
      deltaPct: r.deltaPct,
      baselineCv: r.baselineCv,
      currentCv: r.currentCv,
      regression: r.regression,
      unstable: r.unstable,
    })),
    regressions: regressions.map((r) => ({
      scenarioId: r.scenarioId,
      metric: r.metric,
      deltaPct: r.deltaPct,
    })),
  };

  await Promise.all([
    fs.writeFile(path.join(outDir, "compare.md"), `${reportLines.join("\n")}\n`, "utf8"),
    fs.writeFile(path.join(outDir, "summary.json"), `${JSON.stringify(summary, null, 2)}\n`, "utf8"),
  ]);

  if (result.status === "unstable") {
    process.exitCode = 2;
  } else if (result.status === "fail") {
    process.exitCode = 1;
  } else {
    process.exitCode = 0;
  }
}

if (import.meta.url === `file://${process.argv[1]}`) {
  main().catch((err) => {
    console.error(err?.stack ?? String(err));
    process.exitCode = 1;
  });
}
