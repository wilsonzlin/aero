/**
 * Compare two GPU benchmark reports and fail on regressions/instability.
 *
 * Output:
 *   - <out-dir>/compare.md
 *   - <out-dir>/summary.json
 *
 * Exit codes:
 *   0 = pass
 *   1 = regression
 *   2 = unstable (extreme variance / missing required metrics)
 */

import fs from "node:fs/promises";
import path from "node:path";
import process from "node:process";

import { buildCompareResult, exitCodeForStatus, renderCompareMarkdown } from "../tools/perf/lib/compare_core.mjs";
import {
  DEFAULT_PROFILE,
  DEFAULT_THRESHOLDS_FILE,
  getSuiteThresholds,
  loadThresholdPolicy,
  pickThresholdProfile,
} from "../tools/perf/lib/thresholds.mjs";

function usage(exitCode: number) {
  const msg = `
Usage:
  node --experimental-strip-types scripts/compare_gpu_benchmarks.ts --baseline <gpu_bench.json> --candidate <gpu_bench.json> --out-dir <dir>

Options:
  --baseline <path>          Baseline GPU bench report (required)
  --candidate <path>         Candidate GPU bench report (required)
  --out-dir <dir>            Output directory (required)
  --thresholds-file <path>   Threshold policy file (default: ${DEFAULT_THRESHOLDS_FILE})
  --profile <name>           Threshold profile (default: ${DEFAULT_PROFILE})

Compatibility / override flags (optional):
  --current <path>           Alias for --candidate
  --outDir <dir>             Alias for --out-dir
  --thresholdPct <n>         Override maxRegressionPct for all GPU metrics (percent)
  --cvThreshold <n>          Override extremeCvThreshold for all GPU metrics

Environment overrides (optional):
  GPU_PERF_REGRESSION_THRESHOLD_PCT=15
  GPU_PERF_EXTREME_CV_THRESHOLD=0.5
`;
  console.log(msg.trim());
  process.exit(exitCode);
}

function parseArgs(argv: string[]): Record<string, string> {
  const out: Record<string, string> = {
    "thresholds-file": DEFAULT_THRESHOLDS_FILE,
    profile: DEFAULT_PROFILE,
  };
  for (let i = 0; i < argv.length; i += 1) {
    const a = argv[i];
    if (!a?.startsWith("--")) continue;
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

function isFiniteNumber(n: unknown): n is number {
  return typeof n === "number" && Number.isFinite(n);
}

async function readJson(file: string): Promise<any> {
  return JSON.parse(await fs.readFile(file, "utf8"));
}

function unitForMetric(metric: string): string {
  if (metric.endsWith("MsMean") || metric.endsWith("MsP95") || metric.endsWith("MsP50")) return "ms";
  if (metric === "textureUploadMBpsAvg") return "MB/s";
  if (metric === "fpsAvg") return "fps";
  if (metric === "pipelineCacheHitRate") return "";
  return "";
}

/**
 * @param {any} histogram
 */
function histogramCv(histogram: any): number | null {
  const stats = histogram?.stats ?? null;
  const buckets = histogram?.buckets ?? null;
  const bucketSize = histogram?.bucketSize;
  const min = histogram?.min;
  const max = histogram?.max;
  const underflow = histogram?.underflow ?? 0;
  const overflow = histogram?.overflow ?? 0;

  if (!stats || !Array.isArray(buckets) || !isFiniteNumber(bucketSize) || !isFiniteNumber(min) || !isFiniteNumber(max)) {
    return null;
  }

  const count = stats.count;
  const mean = stats.mean;
  if (!isFiniteNumber(count) || count <= 0 || !isFiniteNumber(mean) || mean === 0) return null;

  let sumSq = 0;
  if (underflow > 0) sumSq += underflow * (min - mean) * (min - mean);
  if (overflow > 0) sumSq += overflow * (max - mean) * (max - mean);

  for (let i = 0; i < buckets.length; i += 1) {
    const n = buckets[i];
    if (!n) continue;
    const x = min + (i + 0.5) * bucketSize;
    const d = x - mean;
    sumSq += n * d * d;
  }

  const variance = sumSq / count;
  const stdev = Math.sqrt(variance);
  return stdev / mean;
}

function extractPrimaryMetricsLegacy(scenario: any): Record<string, number | null> {
  const d = scenario?.derived ?? {};
  const t = scenario?.telemetry ?? {};
  const ft = t.frameTimeMs?.stats ?? {};
  const present = t.presentLatencyMs?.stats ?? {};
  const dxbc = t.shaderTranslationMs?.stats ?? {};
  const wgsl = t.shaderCompilationMs?.stats ?? {};

  return {
    fpsAvg: d.fpsAvg ?? (isFiniteNumber(ft.mean) ? 1000 / ft.mean : null),
    frameTimeMsP50: d.frameTimeMsP50 ?? ft.p50 ?? null,
    frameTimeMsP95: d.frameTimeMsP95 ?? ft.p95 ?? null,
    presentLatencyMsP95: d.presentLatencyMsP95 ?? present.p95 ?? null,
    shaderTranslationMsMean: d.shaderTranslationMsMean ?? dxbc.mean ?? null,
    shaderCompilationMsMean: d.shaderCompilationMsMean ?? wgsl.mean ?? null,
    textureUploadMBpsAvg: d.textureUploadMBpsAvg ?? null,
    pipelineCacheHitRate: d.pipelineCacheHitRate ?? t.pipelineCache?.hitRate ?? null,
  };
}

function cvForMetricLegacy(scenario: any, metricName: string): number | null {
  const t = scenario?.telemetry ?? {};
  switch (metricName) {
    case "frameTimeMsP95":
    case "frameTimeMsP50":
      return histogramCv(t.frameTimeMs);
    case "presentLatencyMsP95":
      return histogramCv(t.presentLatencyMs);
    case "shaderTranslationMsMean":
      return histogramCv(t.shaderTranslationMs);
    case "shaderCompilationMsMean":
      return histogramCv(t.shaderCompilationMs);
    case "textureUploadMBpsAvg":
      return histogramCv(t.textureUpload?.bytesPerFrame);
    default:
      return null;
  }
}

function nForMetricLegacy(scenario: any, metricName: string): number | null {
  const t = scenario?.telemetry ?? {};
  const pickCount = (h: any) =>
    typeof h?.stats?.count === "number" && Number.isFinite(h.stats.count) ? h.stats.count : null;
  switch (metricName) {
    case "frameTimeMsP95":
    case "frameTimeMsP50":
      return pickCount(t.frameTimeMs);
    case "presentLatencyMsP95":
      return pickCount(t.presentLatencyMs);
    case "shaderTranslationMsMean":
      return pickCount(t.shaderTranslationMs);
    case "shaderCompilationMsMean":
      return pickCount(t.shaderCompilationMs);
    case "textureUploadMBpsAvg":
      return pickCount(t.textureUpload?.bytesPerFrame);
    default:
      return null;
  }
}

function statsFromScenario(scenario: any, metricName: string): { value: number; cv: number | null; n: number | null } | null {
  const metricStats = scenario?.metrics?.[metricName];
  if (metricStats && typeof metricStats === "object") {
    const value = isFiniteNumber(metricStats.median)
      ? metricStats.median
      : isFiniteNumber(metricStats.p50)
        ? metricStats.p50
        : null;
    if (!isFiniteNumber(value)) return null;
    const cv = isFiniteNumber(metricStats.cv) ? metricStats.cv : null;
    const n = isFiniteNumber(metricStats.n)
      ? metricStats.n
      : isFiniteNumber(metricStats.count)
        ? metricStats.count
        : null;
    return { value, cv, n };
  }

  const legacy = extractPrimaryMetricsLegacy(scenario);
  const value = legacy[metricName];
  if (!isFiniteNumber(value)) return null;
  return { value, cv: cvForMetricLegacy(scenario, metricName), n: nForMetricLegacy(scenario, metricName) };
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  if (args.help === "true") usage(0);

  const baselinePath = args.baseline;
  const candidatePath = args.candidate ?? args.current;
  const outDir = args["out-dir"] ?? args.outDir;
  const thresholdsFile = args["thresholds-file"] ?? DEFAULT_THRESHOLDS_FILE;
  const profileArg = args.profile ?? DEFAULT_PROFILE;

  if (!baselinePath || !candidatePath || !outDir) {
    console.error("--baseline, --candidate, and --out-dir are required");
    usage(1);
  }

  await fs.mkdir(outDir, { recursive: true });

  const [baseline, candidate] = await Promise.all([readJson(baselinePath), readJson(candidatePath)]);

  const thresholdsPolicy = await loadThresholdPolicy(thresholdsFile);
  const { name: profileName, profile } = pickThresholdProfile(thresholdsPolicy, profileArg);
  const suiteThresholds = getSuiteThresholds(profile, "gpu");

  const baseScenarios = baseline?.summary?.scenarios ?? baseline?.scenarios ?? {};
  const candScenarios = candidate?.summary?.scenarios ?? candidate?.scenarios ?? {};

  const scenarioIds = new Set([...Object.keys(baseScenarios), ...Object.keys(candScenarios)]);
  const cases: any[] = [];

  for (const scenarioId of Array.from(scenarioIds).sort()) {
    const b = baseScenarios[scenarioId];
    const c = candScenarios[scenarioId];
    const scenarioLabel = c?.name ?? b?.name ?? scenarioId;
    const bOk = b?.status === "ok";
    const cOk = c?.status === "ok";

    for (const [metricName, threshold] of Object.entries(suiteThresholds.metrics ?? {})) {
      const better = (threshold as any)?.better;
      if (better !== "lower" && better !== "higher") {
        throw new Error(`thresholds: gpu.metrics.${metricName}.better must be "lower" or "higher"`);
      }

      const baselineStats = bOk ? statsFromScenario(b, metricName) : null;
      const candidateStats = cOk ? statsFromScenario(c, metricName) : null;

      cases.push({
        scenario: scenarioLabel,
        metric: metricName,
        unit: unitForMetric(metricName),
        better,
        threshold,
        baseline: baselineStats,
        candidate: candidateStats,
      });
    }
  }

  // Optional overrides (useful for debugging / emergency CI tuning).
  const cliThresholdPct = args.thresholdPct ? Number(args.thresholdPct) / 100 : null;
  const cliCvThreshold = args.cvThreshold ? Number(args.cvThreshold) : null;
  const envThresholdPct = process.env.GPU_PERF_REGRESSION_THRESHOLD_PCT
    ? Number(process.env.GPU_PERF_REGRESSION_THRESHOLD_PCT) / 100
    : null;
  const envExtremeCv = process.env.GPU_PERF_EXTREME_CV_THRESHOLD
    ? Number(process.env.GPU_PERF_EXTREME_CV_THRESHOLD)
    : null;

  const overrideMaxRegressionPct =
    (isFiniteNumber(cliThresholdPct) && cliThresholdPct > 0 && cliThresholdPct) ||
    (isFiniteNumber(envThresholdPct) && envThresholdPct > 0 && envThresholdPct) ||
    null;
  const overrideExtremeCv =
    (isFiniteNumber(cliCvThreshold) && cliCvThreshold > 0 && cliCvThreshold) ||
    (isFiniteNumber(envExtremeCv) && envExtremeCv > 0 && envExtremeCv) ||
    null;

  if (overrideMaxRegressionPct != null || overrideExtremeCv != null) {
    for (const c of cases) {
      if (overrideMaxRegressionPct != null) c.threshold = { ...c.threshold, maxRegressionPct: overrideMaxRegressionPct };
      if (overrideExtremeCv != null) c.threshold = { ...c.threshold, extremeCvThreshold: overrideExtremeCv };
    }
  }

  const result = buildCompareResult({
    suite: "gpu",
    profile: profileName,
    thresholdsFile,
    baselineMeta: baseline?.meta ?? baseline?.summary?.meta ?? null,
    candidateMeta: candidate?.meta ?? candidate?.summary?.meta ?? null,
    cases,
  });

  const markdown = renderCompareMarkdown(result, { title: "GPU perf comparison" });
  await Promise.all([
    fs.writeFile(path.join(outDir, "compare.md"), markdown),
    fs.writeFile(path.join(outDir, "summary.json"), JSON.stringify(result, null, 2)),
  ]);

  process.exitCode = exitCodeForStatus(result.status);
}

if (import.meta.url === `file://${process.argv[1]}`) {
  main().catch((err) => {
    console.error(err?.stack ?? String(err));
    process.exitCode = 1;
  });
}

