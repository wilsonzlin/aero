/**
 * Compare two aero-gateway benchmark reports and fail on regressions/instability.
 *
 * Output:
 *   - <out-dir>/compare.md
 *   - <out-dir>/summary.json
 *
 * Exit codes:
 *   0 = pass
 *   1 = regression
 *   2 = unstable (extreme variance detected)
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
import { normaliseBenchResult } from "../bench/history.js";
import { formatOneLineError, truncateUtf8 } from "../src/text.js";

function usage(exitCode: number) {
  const msg = `
Usage:
  node --experimental-strip-types --import ./scripts/register-ts-strip-loader.mjs scripts/compare_gateway_benchmarks.ts --baseline <gateway.json> --candidate <gateway.json> --out-dir <dir>

Options:
  --baseline <path>          Baseline gateway bench report (required)
  --candidate <path>         Candidate gateway bench report (required; alias: --current)
  --current <path>           Alias for --candidate
  --out-dir <dir>            Output directory (required; alias: --outDir)
  --outDir <dir>             Alias for --out-dir
  --thresholds-file <path>   Threshold policy file (default: ${DEFAULT_THRESHOLDS_FILE})
  --profile <name>           Threshold profile (default: ${DEFAULT_PROFILE})

Override flags (optional):
  --thresholdPct <n>         Override maxRegressionPct for all gateway metrics (percent)
  --cvThreshold <n>          Override extremeCvThreshold for all gateway metrics

Environment overrides (optional):
  GATEWAY_PERF_REGRESSION_THRESHOLD_PCT=15
  GATEWAY_PERF_EXTREME_CV_THRESHOLD=0.5
`.trim();
  console.log(msg);
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

function coerceScalarString(value: unknown): string {
  if (value == null) return "";
  switch (typeof value) {
    case "string":
      return value;
    case "number":
    case "boolean":
    case "bigint":
      return String(value);
    default:
      return "";
  }
}

async function readJson(file: string): Promise<any> {
  return JSON.parse(await fs.readFile(file, "utf8"));
}

function metricStatsFromHistoryMetric(metric: any): { value: number; cv: number | null; n: number | null } | null {
  if (!metric || typeof metric !== "object") return null;
  const value = metric.value;
  if (!isFiniteNumber(value)) return null;
  const cv = isFiniteNumber(metric.samples?.cv) ? metric.samples.cv : null;
  const n = isFiniteNumber(metric.samples?.n) ? metric.samples.n : null;
  return { value, cv, n };
}

export function compareGatewayBenchmarks({
  baselineRaw,
  candidateRaw,
  suiteThresholds,
  thresholdsFile,
  profileName,
  overrideMaxRegressionPct = null,
  overrideExtremeCv = null,
}: {
  baselineRaw: any;
  candidateRaw: any;
  suiteThresholds: any;
  thresholdsFile: string;
  profileName: string;
  overrideMaxRegressionPct?: number | null;
  overrideExtremeCv?: number | null;
}) {
  const baseline = normaliseBenchResult(baselineRaw);
  const candidate = normaliseBenchResult(candidateRaw);

  const baseGateway = baseline.scenarios?.gateway?.metrics ?? {};
  const candGateway = candidate.scenarios?.gateway?.metrics ?? {};

  const cases: any[] = [];
  for (const [metricName, threshold] of Object.entries(suiteThresholds.metrics ?? {})) {
    const better = (threshold as { better?: unknown } | null | undefined)?.better;
    if (better !== "lower" && better !== "higher") {
      throw new Error(`thresholds: gateway.metrics.${metricName}.better must be "lower" or "higher"`);
    }

    const b = (baseGateway as Record<string, unknown>)[metricName];
    const c = (candGateway as Record<string, unknown>)[metricName];
    const unit = coerceScalarString((b as any)?.unit ?? (c as any)?.unit).slice(0, 64);

    let effectiveThreshold: any = threshold;
    if (overrideMaxRegressionPct != null) {
      effectiveThreshold = { ...effectiveThreshold, maxRegressionPct: overrideMaxRegressionPct };
    }
    if (overrideExtremeCv != null) {
      effectiveThreshold = { ...effectiveThreshold, extremeCvThreshold: overrideExtremeCv };
    }

    cases.push({
      scenario: "gateway",
      metric: metricName,
      unit,
      better,
      threshold: effectiveThreshold,
      baseline: metricStatsFromHistoryMetric(b),
      candidate: metricStatsFromHistoryMetric(c),
    });
  }

  const result: any = buildCompareResult({
    suite: "gateway",
    profile: profileName,
    thresholdsFile,
    baselineMeta: baseline.environment ?? null,
    candidateMeta: candidate.environment ?? null,
    cases,
  });

  // Treat missing candidate metrics as unstable. This avoids silently passing
  // when a benchmark run fails or a metric stops being reported.
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

  const [baselineRaw, candidateRaw] = await Promise.all([readJson(baselinePath), readJson(candidatePath)]);

  const thresholdsPolicy = await loadThresholdPolicy(thresholdsFile);
  const { name: profileName, profile } = pickThresholdProfile(thresholdsPolicy, profileArg);
  const suiteThresholds = getSuiteThresholds(profile, "gateway");

  // Optional overrides (useful for debugging / emergency CI tuning).
  const cliThresholdPct = args.thresholdPct ? Number(args.thresholdPct) / 100 : null;
  const cliCvThreshold = args.cvThreshold ? Number(args.cvThreshold) : null;
  const envThresholdPct = process.env.GATEWAY_PERF_REGRESSION_THRESHOLD_PCT
    ? Number(process.env.GATEWAY_PERF_REGRESSION_THRESHOLD_PCT) / 100
    : null;
  const envExtremeCv = process.env.GATEWAY_PERF_EXTREME_CV_THRESHOLD
    ? Number(process.env.GATEWAY_PERF_EXTREME_CV_THRESHOLD)
    : null;

  const overrideMaxRegressionPct =
    (isFiniteNumber(cliThresholdPct) && cliThresholdPct > 0 && cliThresholdPct) ||
    (isFiniteNumber(envThresholdPct) && envThresholdPct > 0 && envThresholdPct) ||
    null;
  const overrideExtremeCv =
    (isFiniteNumber(cliCvThreshold) && cliCvThreshold > 0 && cliCvThreshold) ||
    (isFiniteNumber(envExtremeCv) && envExtremeCv > 0 && envExtremeCv) ||
    null;

  const result = compareGatewayBenchmarks({
    baselineRaw,
    candidateRaw,
    suiteThresholds,
    thresholdsFile,
    profileName,
    overrideMaxRegressionPct,
    overrideExtremeCv,
  });

  const markdown = renderCompareMarkdown(result, { title: "Gateway perf comparison" });
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
