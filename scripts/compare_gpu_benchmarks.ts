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
import { formatOneLineError, truncateUtf8 } from "../src/text.js";

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

function mdInlineCode(value: unknown): string {
  if (value === null || value === undefined) return "—";
  const raw =
    typeof value === "string"
      ? value
      : typeof value === "number" || typeof value === "boolean"
        ? String(value)
        : JSON.stringify(value);
  return `\`${raw.replaceAll("`", "\\`").replaceAll("|", "\\|")}\``;
}

function injectContextIntoMarkdown(params: { markdown: string; baseline: any; candidate: any }): string {
  const mdRaw = params.markdown;
  if (mdRaw.includes("\n## Context\n") || mdRaw.startsWith("## Context\n")) return mdRaw;

  const baselineMeta = params.baseline?.meta ?? params.baseline?.summary?.meta ?? null;
  const candidateMeta = params.candidate?.meta ?? params.candidate?.summary?.meta ?? null;
  const baselineEnv = params.baseline?.environment ?? null;
  const candidateEnv = params.candidate?.environment ?? null;

  const rows: Array<[string, unknown, unknown]> = [
    ["schemaVersion", params.baseline?.schemaVersion, params.candidate?.schemaVersion],
    ["meta.iterations", baselineMeta?.iterations, candidateMeta?.iterations],
    ["meta.nodeVersion", baselineMeta?.nodeVersion, candidateMeta?.nodeVersion],
    ["environment.webgpu", baselineEnv?.webgpu, candidateEnv?.webgpu],
    ["environment.webgl2", baselineEnv?.webgl2, candidateEnv?.webgl2],
    ["environment.userAgent", baselineEnv?.userAgent, candidateEnv?.userAgent],
  ];

  const baseScenarios = params.baseline?.summary?.scenarios ?? params.baseline?.scenarios ?? {};
  const candScenarios = params.candidate?.summary?.scenarios ?? params.candidate?.scenarios ?? {};
  const scenarioIds = new Set([...Object.keys(baseScenarios), ...Object.keys(candScenarios)]);

  const contextLines: string[] = [];
  contextLines.push("## Context");
  contextLines.push("");
  contextLines.push("| Field | Baseline | Candidate |");
  contextLines.push("| --- | --- | --- |");
  for (const [field, b, c] of rows) {
    contextLines.push(`| ${field} | ${mdInlineCode(b)} | ${mdInlineCode(c)} |`);
  }
  contextLines.push("");

  if (scenarioIds.size > 0) {
    contextLines.push("### Scenario status");
    contextLines.push("");
    contextLines.push("| Scenario | Baseline | Candidate |");
    contextLines.push("| --- | --- | --- |");

    for (const id of Array.from(scenarioIds).sort()) {
      const b = baseScenarios?.[id];
      const c = candScenarios?.[id];
      const label = (c?.name ?? b?.name ?? id).replaceAll("|", "\\|");

      const bStatus = b?.status ?? "missing";
      const cStatus = c?.status ?? "missing";
      const bApi = b?.api ?? null;
      const cApi = c?.api ?? null;

      const bCell = `${bStatus}${bApi ? ` (${bApi})` : ""}`;
      const cCell = `${cStatus}${cApi ? ` (${cApi})` : ""}`;
      contextLines.push(`| ${label} | ${mdInlineCode(bCell)} | ${mdInlineCode(cCell)} |`);
    }

    contextLines.push("");
  }

  const mdLines = mdRaw.split(/\r?\n/u);
  const summaryIdx = mdLines.findIndex((line) => line.trim() === "## Summary");
  if (summaryIdx === -1) return mdRaw;

  mdLines.splice(summaryIdx, 0, ...contextLines);
  return `${mdLines.join("\n")}\n`;
}

function unitForMetric(metric: string): string {
  if (metric.endsWith("MsMean") || metric.endsWith("MsP95") || metric.endsWith("MsP50")) return "ms";
  if (metric === "textureUploadMBpsAvg") return "MB/s";
  if (metric === "fpsAvg") return "fps";
  if (metric === "pipelineCacheHitRate") return "";
  return "";
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
  // Legacy aero-gpu-bench reports (schema v1) only provide a single derived value
  // per scenario. We treat these as a single-sample measurement (n=1, cv=0),
  // matching how other perf tooling treats one-iteration runs.
  return { value, cv: 0, n: 1 };
}

export function compareGpuBenchmarks({
  baseline,
  candidate,
  suiteThresholds,
  thresholdsFile,
  profileName,
  overrideMaxRegressionPct = null,
  overrideExtremeCv = null,
}: {
  baseline: any;
  candidate: any;
  suiteThresholds: any;
  thresholdsFile: string;
  profileName: string;
  overrideMaxRegressionPct?: number | null;
  overrideExtremeCv?: number | null;
}) {
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
      const better = (threshold as { better?: unknown } | null | undefined)?.better;
      if (better !== "lower" && better !== "higher") {
        throw new Error(`thresholds: gpu.metrics.${metricName}.better must be "lower" or "higher"`);
      }

      const baselineStats = bOk ? statsFromScenario(b, metricName) : null;
      const candidateStats = cOk ? statsFromScenario(c, metricName) : null;

      // Some GPU metrics are only recorded by certain scenarios (or only when a
      // scenario runs via a particular API backend). If a metric is missing in
      // both baseline and candidate for an otherwise-successful scenario, treat
      // it as "not applicable" and skip the comparison.
      //
      // We intentionally *do not* skip missing metrics when either scenario is
      // non-`ok` (skipped/error), so scenario failures still surface as
      // instability in CI.
      if (bOk && cOk && !baselineStats && !candidateStats) {
        continue;
      }

      let effectiveThreshold: any = threshold;
      if (overrideMaxRegressionPct != null) {
        effectiveThreshold = { ...effectiveThreshold, maxRegressionPct: overrideMaxRegressionPct };
      }
      if (overrideExtremeCv != null) {
        effectiveThreshold = { ...effectiveThreshold, extremeCvThreshold: overrideExtremeCv };
      }

      cases.push({
        scenario: scenarioLabel,
        metric: metricName,
        unit: unitForMetric(metricName),
        better,
        threshold: effectiveThreshold,
        baseline: baselineStats,
        candidate: candidateStats,
      });
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

  // Treat missing candidate metrics as unstable. This avoids silently passing
  // when a benchmark scenario fails or a metric stops being reported.
  //
  // Note: missing baseline metrics are treated as unstable by compare_core for
  // non-informational metrics. If you want to roll out a new metric without
  // breaking comparisons, mark it informational until the baseline also
  // produces it (or avoid adding it to the threshold policy yet).
  const missingCandidateRequired = (result.comparisons ?? []).some(
    (c: any) => c?.status === "missing_candidate" && !c?.informational,
  );
  if (missingCandidateRequired) {
    result.status = "unstable";
  }

  return result;
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

  const result = compareGpuBenchmarks({
    baseline,
    candidate,
    suiteThresholds,
    thresholdsFile,
    profileName,
    overrideMaxRegressionPct,
    overrideExtremeCv,
  });

  const markdown = injectContextIntoMarkdown({
    markdown: renderCompareMarkdown(result, { title: "GPU perf comparison" }),
    baseline,
    candidate,
  });
  await Promise.all([
    fs.writeFile(path.join(outDir, "compare.md"), markdown),
    fs.writeFile(path.join(outDir, "summary.json"), JSON.stringify(result, null, 2)),
  ]);

  process.exitCode = exitCodeForStatus(result.status);
}

if (import.meta.url === `file://${process.argv[1]}`) {
  main().catch((err) => {
    let stack: string | null = null;
    if (err && typeof err === "object") {
      try {
        const raw = (err as { stack?: unknown }).stack;
        if (typeof raw === "string" && raw) stack = raw;
      } catch {
        // ignore getters throwing
      }
    }
    console.error(stack ? truncateUtf8(stack, 8 * 1024) : formatOneLineError(err, 512));
    process.exitCode = 1;
  });
}
