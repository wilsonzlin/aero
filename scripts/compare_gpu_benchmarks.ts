/**
 * Compare two GPU benchmark reports and fail on regressions.
 *
 * Usage:
 *   node --experimental-strip-types scripts/compare_gpu_benchmarks.ts \
 *     --baseline path/to/baseline.json \
 *     --current path/to/current.json \
 *     --thresholdPct 5
 *
 * The script exits with code 1 if any primary metric regresses by more than the
 * given threshold percentage.
 */

import fs from "node:fs/promises";

function parseArgs(argv) {
  /** @type {Record<string, string>} */
  const out = {};
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

/**
 * @param {any} n
 */
function isFiniteNumber(n) {
  return typeof n === "number" && Number.isFinite(n);
}

/**
 * @param {number} value
 */
function fmtPct(value) {
  return `${(value * 100).toFixed(2)}%`;
}

/**
 * @param {number} value
 */
function fmtSignedPct(value) {
  const sign = value >= 0 ? "+" : "";
  return `${sign}${fmtPct(value)}`;
}

/**
 * @param {any} scenario
 */
function extractPrimaryMetrics(scenario) {
  const d = scenario?.derived ?? {};
  const t = scenario?.telemetry ?? {};
  const ft = t.frameTimeMs?.stats ?? {};
  const present = t.presentLatencyMs?.stats ?? {};
  const dxbc = t.shaderTranslationMs?.stats ?? {};
  const wgsl = t.shaderCompilationMs?.stats ?? {};

  return {
    fpsAvg: d.fpsAvg ?? (isFiniteNumber(ft.mean) ? 1000 / ft.mean : null),
    frameTimeMsP95: d.frameTimeMsP95 ?? ft.p95 ?? null,
    presentLatencyMsP95: d.presentLatencyMsP95 ?? present.p95 ?? null,
    shaderTranslationMsMean: d.shaderTranslationMsMean ?? dxbc.mean ?? null,
    shaderCompilationMsMean: d.shaderCompilationMsMean ?? wgsl.mean ?? null,
    textureUploadMBpsAvg: d.textureUploadMBpsAvg ?? null,
    pipelineCacheHitRate: d.pipelineCacheHitRate ?? t.pipelineCache?.hitRate ?? null,
  };
}

/**
 * @param {{name: string, baseline: number, current: number, better: "lower"|"higher", threshold: number}} cfg
 */
function compareMetric(cfg) {
  const delta = (cfg.current - cfg.baseline) / cfg.baseline;
  const regression =
    cfg.better === "lower" ? delta > cfg.threshold : delta < -cfg.threshold;
  return { delta, regression };
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  const baselinePath = args.baseline;
  const currentPath = args.current;
  const thresholdPct = args.thresholdPct ? Number(args.thresholdPct) : 5;
  const threshold = thresholdPct / 100;

  if (!baselinePath || !currentPath) {
    throw new Error("Missing required args: --baseline <path> --current <path>");
  }
  if (!Number.isFinite(threshold) || threshold <= 0) {
    throw new Error("--thresholdPct must be a positive number");
  }

  const baseline = JSON.parse(await fs.readFile(baselinePath, "utf8"));
  const current = JSON.parse(await fs.readFile(currentPath, "utf8"));

  const baseScenarios = baseline?.scenarios ?? {};
  const curScenarios = current?.scenarios ?? {};

  /** @type {{scenarioId:string, metric:string, delta:number, baseline:number, current:number}[]} */
  const regressions = [];

  const scenarioIds = new Set([...Object.keys(baseScenarios), ...Object.keys(curScenarios)]);
  for (const scenarioId of Array.from(scenarioIds).sort()) {
    const b = baseScenarios[scenarioId];
    const c = curScenarios[scenarioId];
    if (!b || !c) continue;
    if (b.status !== "ok" || c.status !== "ok") continue;

    const bm = extractPrimaryMetrics(b);
    const cm = extractPrimaryMetrics(c);

    /** @type {Array<[string, "lower"|"higher"]>} */
    const metricsToCheck = [
      // Primary signal: frame pacing / throughput.
      ["frameTimeMsP95", "lower"],
      // Translation pipeline: tends to be stable and is a common regression source.
      ["shaderTranslationMsMean", "lower"],
      ["shaderCompilationMsMean", "lower"],
      // Secondary signals.
      ["presentLatencyMsP95", "lower"],
      ["textureUploadMBpsAvg", "higher"],
      ["pipelineCacheHitRate", "higher"],
    ];

    for (const [metric, better] of metricsToCheck) {
      const baseVal = bm[metric];
      const curVal = cm[metric];
      if (!isFiniteNumber(baseVal) || !isFiniteNumber(curVal) || baseVal === 0) {
        continue;
      }
      const { delta, regression } = compareMetric({
        name: metric,
        baseline: baseVal,
        current: curVal,
        better,
        threshold,
      });
      if (regression) {
        regressions.push({ scenarioId, metric, delta, baseline: baseVal, current: curVal });
      }
    }
  }

  if (regressions.length === 0) {
    console.log(`OK: no regressions beyond ${thresholdPct}%`);
    return;
  }

  console.error(`FAIL: ${regressions.length} regressions beyond ${thresholdPct}%`);
  for (const r of regressions) {
    console.error(
      `- ${r.scenarioId}.${r.metric}: baseline=${r.baseline} current=${r.current} (${fmtSignedPct(r.delta)})`,
    );
  }
  process.exitCode = 1;
}

if (import.meta.url === `file://${process.argv[1]}`) {
  main().catch((err) => {
    console.error(err?.stack ?? String(err));
    process.exitCode = 1;
  });
}

