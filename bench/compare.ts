import fs from "node:fs/promises";
import path from "node:path";
import process from "node:process";

import { buildCompareResult, exitCodeForStatus, renderCompareMarkdown, statsFromSamples } from "../tools/perf/lib/compare_core.mjs";
import {
  DEFAULT_PROFILE,
  DEFAULT_THRESHOLDS_FILE,
  getSuiteThresholds,
  loadThresholdPolicy,
  pickThresholdProfile,
} from "../tools/perf/lib/thresholds.mjs";
import { formatOneLineError, truncateUtf8 } from "../src/text.js";

export type StorageContextStatus = "ok" | "warn" | "fail";

export interface StorageContextCheck {
  field: string;
  baseline: string | null;
  candidate: string | null;
  status: StorageContextStatus;
  note?: string;
}

const OPTIONAL_STORAGE_METRICS = new Set(["random_write_4k_p95_ms"]);

function usage(exitCode: number) {
  const msg = `
Usage:
  node --experimental-strip-types bench/compare.ts --baseline <storage_bench.json> --candidate <storage_bench.json> --out-dir <dir>

Options:
  --baseline <path>          Baseline storage_bench.json (required)
  --candidate <path>         Candidate storage_bench.json (required)
  --out-dir <dir>            Output directory (required)
  --thresholds-file <path>   Threshold policy file (default: ${DEFAULT_THRESHOLDS_FILE})
  --profile <name>           Threshold profile (default: ${DEFAULT_PROFILE})
  --help                     Show this help

Compatibility / override flags (optional):
  --current <path>           Alias for --candidate
  --outDir <dir>             Alias for --out-dir
  --thresholdPct <n>         Override maxRegressionPct for all storage metrics (percent)
  --cvThreshold <n>          Override extremeCvThreshold for all storage metrics

Environment overrides (optional):
  STORAGE_PERF_REGRESSION_THRESHOLD_PCT=15
  STORAGE_PERF_EXTREME_CV_THRESHOLD=0.5
`;
  console.log(msg.trim());
  process.exit(exitCode);
}

function parseArgs(argv: string[]) {
  const out: Record<string, string> = {
    "thresholds-file": DEFAULT_THRESHOLDS_FILE,
    profile: DEFAULT_PROFILE,
  };

  for (let i = 0; i < argv.length; i += 1) {
    const a = argv[i]!;
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

function mdEscape(text: unknown): string {
  return coerceScalarString(text).replaceAll("|", "\\|");
}

async function readJson(file: string): Promise<any> {
  return JSON.parse(await fs.readFile(file, "utf8"));
}

function getString(obj: any, getter: (v: any) => unknown): string | null {
  const value = getter(obj);
  return typeof value === "string" && value.length > 0 ? value : null;
}

function throughputSamples(result: any, key: "sequential_write" | "sequential_read"): number[] | null {
  const runs = result?.[key]?.runs;
  if (!Array.isArray(runs)) return null;
  const values = runs.map((r) => r?.mb_per_s).filter(isFiniteNumber);
  return values.length ? values : null;
}

function latencyP95Samples(result: any, key: "random_read_4k" | "random_write_4k"): number[] | null {
  const runs = result?.[key]?.runs;
  if (!Array.isArray(runs)) return null;
  const values = runs.map((r) => r?.p95_ms).filter(isFiniteNumber);
  return values.length ? values : null;
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
    const value = (cfg as Record<string, unknown>)[key];
    const rendered =
      value === undefined
        ? "unset"
        : value === null
          ? "null"
          : typeof value === "string" || typeof value === "number" || typeof value === "boolean" || typeof value === "bigint"
            ? String(value)
            : "<non-scalar>";
    parts.push(`config.${key}=${rendered}`);
  }
  return parts.join(" ");
}

function compareOrderedField(params: {
  field: string;
  baseline: string | null;
  candidate: string | null;
  ranks: Record<string, number>;
}): StorageContextCheck {
  if (!params.baseline || !params.candidate) {
    return {
      field: params.field,
      baseline: params.baseline,
      candidate: params.candidate,
      status: "fail",
      note: "missing/invalid baseline or candidate value",
    };
  }

  if (params.baseline === params.candidate) {
    return { field: params.field, baseline: params.baseline, candidate: params.candidate, status: "ok" };
  }

  const baseRank = params.ranks[params.baseline];
  const candRank = params.ranks[params.candidate];
  if (Number.isFinite(baseRank) && Number.isFinite(candRank)) {
    const regression = candRank < baseRank;
    return {
      field: params.field,
      baseline: params.baseline,
      candidate: params.candidate,
      status: regression ? "fail" : "warn",
      note: regression ? "capability regressed" : "capability improved/changed",
    };
  }

  return {
    field: params.field,
    baseline: params.baseline,
    candidate: params.candidate,
    status: "fail",
    note: "unknown baseline/candidate value",
  };
}

function compareConfig(baseline: any, candidate: any): StorageContextCheck {
  const baselineCfg = baseline?.config;
  const candidateCfg = candidate?.config;

  const baselineSummary = formatConfigSummary(baselineCfg);
  const candidateSummary = formatConfigSummary(candidateCfg);

  if (!baselineCfg || typeof baselineCfg !== "object" || Array.isArray(baselineCfg)) {
    return {
      field: "config",
      baseline: baselineSummary,
      candidate: candidateSummary,
      status: "fail",
      note: "missing/invalid baseline config",
    };
  }
  if (!candidateCfg || typeof candidateCfg !== "object" || Array.isArray(candidateCfg)) {
    return {
      field: "config",
      baseline: baselineSummary,
      candidate: candidateSummary,
      status: "fail",
      note: "missing/invalid candidate config",
    };
  }

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
  const baselineRec = baselineCfg as Record<string, unknown>;
  const candidateRec = candidateCfg as Record<string, unknown>;
  for (const key of keys) {
    const a = baselineRec[key];
    const b = candidateRec[key];
    if (a !== b) diffs.push(`config.${key}: ${a === undefined ? "unset" : a} -> ${b === undefined ? "unset" : b}`);
  }

  if (diffs.length === 0) {
    return { field: "config", baseline: baselineSummary, candidate: candidateSummary, status: "ok" };
  }

  return {
    field: "config",
    baseline: baselineSummary,
    candidate: candidateSummary,
    status: "fail",
    note: diffs.join(", "),
  };
}

export function buildStorageContextChecks(baseline: any, candidate: any): StorageContextCheck[] {
  const checks: StorageContextCheck[] = [];

  const baselineBackend = getString(baseline, (v) => v?.backend);
  const candidateBackend = getString(candidate, (v) => v?.backend);
  checks.push(
    compareOrderedField({
      field: "backend",
      baseline: baselineBackend,
      candidate: candidateBackend,
      ranks: { indexeddb: 1, opfs: 2 },
    }),
  );

  // Only gate api_mode if we stayed on OPFS; IndexedDB is always async.
  if (baselineBackend === "opfs" && candidateBackend === "opfs") {
    const baselineApiMode = getString(baseline, (v) => v?.api_mode);
    const candidateApiMode = getString(candidate, (v) => v?.api_mode);
    checks.push(
      compareOrderedField({
        field: "api_mode",
        baseline: baselineApiMode,
        candidate: candidateApiMode,
        ranks: { async: 1, sync_access_handle: 2 },
      }),
    );
  }

  checks.push(compareConfig(baseline, candidate));
  return checks;
}

function extractWarnings(result: any): string[] {
  const warnings = result?.warnings;
  if (!Array.isArray(warnings)) return [];
  return warnings.filter((w) => typeof w === "string" && w.length > 0);
}

export function renderStorageCompareMarkdown(params: {
  result: any;
  contextChecks: StorageContextCheck[];
  baselineWarnings: string[];
  candidateWarnings: string[];
}): string {
  const reportLines = renderCompareMarkdown(params.result, { title: "Storage perf comparison" }).trimEnd().split("\n");
  const summaryIdx = reportLines.findIndex((line) => line.startsWith("## Summary"));

  const extra: string[] = [];

  if (params.contextChecks.length > 0) {
    extra.push("## Context");
    extra.push("");
    extra.push("| Field | Baseline | Candidate | Status | Note |");
    extra.push("| --- | --- | --- | --- | --- |");
    for (const c of params.contextChecks) {
      const status = c.status === "ok" ? "OK" : c.status === "warn" ? "WARN" : "FAIL";
      extra.push(
        `| ${mdEscape(c.field)} | ${mdEscape(c.baseline ?? "n/a")} | ${mdEscape(c.candidate ?? "n/a")} | ${status} | ${mdEscape(c.note ?? "")} |`,
      );
    }
    extra.push("");
  }

  if (params.baselineWarnings.length > 0 || params.candidateWarnings.length > 0) {
    extra.push("## Warnings");
    extra.push("");
    if (params.baselineWarnings.length > 0) {
      extra.push("Baseline warnings:");
      for (const w of params.baselineWarnings) extra.push(`- ${w}`);
      extra.push("");
    }
    if (params.candidateWarnings.length > 0) {
      extra.push("Candidate warnings:");
      for (const w of params.candidateWarnings) extra.push(`- ${w}`);
      extra.push("");
    }
  }

  if (summaryIdx !== -1 && extra.length > 0) {
    reportLines.splice(summaryIdx, 0, ...extra);
  } else if (extra.length > 0) {
    reportLines.push("", ...extra);
  }

  return `${reportLines.join("\n")}\n`;
}

export function buildStorageCompareResult(params: {
  baseline: any;
  candidate: any;
  thresholdsFile: string;
  profileName: string;
  suiteThresholds: any;
  overrideMaxRegressionPct: number | null;
  overrideExtremeCv: number | null;
}): { result: any; contextChecks: StorageContextCheck[]; baselineWarnings: string[]; candidateWarnings: string[] } {
  /** @type {any[]} */
  const cases: any[] = [];

  for (const [metricName, threshold] of Object.entries(params.suiteThresholds.metrics ?? {})) {
    const better = (threshold as { better?: unknown } | null | undefined)?.better;
    if (better !== "lower" && better !== "higher") {
      throw new Error(`thresholds: storage.metrics.${metricName}.better must be "lower" or "higher"`);
    }

    let baselineSamples: number[] | null = null;
    let candidateSamples: number[] | null = null;
    let unit = "";

    switch (metricName) {
      case "sequential_write_mb_per_s":
        baselineSamples = throughputSamples(params.baseline, "sequential_write");
        candidateSamples = throughputSamples(params.candidate, "sequential_write");
        unit = "MB/s";
        break;
      case "sequential_read_mb_per_s":
        baselineSamples = throughputSamples(params.baseline, "sequential_read");
        candidateSamples = throughputSamples(params.candidate, "sequential_read");
        unit = "MB/s";
        break;
      case "random_read_4k_p95_ms":
        baselineSamples = latencyP95Samples(params.baseline, "random_read_4k");
        candidateSamples = latencyP95Samples(params.candidate, "random_read_4k");
        unit = "ms";
        break;
      case "random_write_4k_p95_ms":
        baselineSamples = latencyP95Samples(params.baseline, "random_write_4k");
        candidateSamples = latencyP95Samples(params.candidate, "random_write_4k");
        unit = "ms";
        break;
      default:
        // Unknown metric in policy; include as missing so it appears in the report.
        baselineSamples = null;
        candidateSamples = null;
        unit = "";
        break;
    }

    if (OPTIONAL_STORAGE_METRICS.has(metricName) && baselineSamples == null && candidateSamples == null) {
      continue;
    }

    const adjustedThreshold = { ...(threshold as Record<string, unknown>) };
    if (params.overrideMaxRegressionPct != null) adjustedThreshold.maxRegressionPct = params.overrideMaxRegressionPct;
    if (params.overrideExtremeCv != null) adjustedThreshold.extremeCvThreshold = params.overrideExtremeCv;

    cases.push({
      scenario: "storage",
      metric: metricName,
      unit,
      better,
      threshold: adjustedThreshold,
      baseline: baselineSamples ? statsFromSamples(baselineSamples) : null,
      candidate: candidateSamples ? statsFromSamples(candidateSamples) : null,
    });
  }

  const result = buildCompareResult({
    suite: "storage",
    profile: params.profileName,
    thresholdsFile: params.thresholdsFile,
    baselineMeta: {
      run_id: params.baseline?.run_id,
      backend: params.baseline?.backend,
      api_mode: params.baseline?.api_mode,
    },
    candidateMeta: {
      run_id: params.candidate?.run_id,
      backend: params.candidate?.backend,
      api_mode: params.candidate?.api_mode,
    },
    cases,
  });

  const contextChecks = buildStorageContextChecks(params.baseline, params.candidate);
  const baselineWarnings = extractWarnings(params.baseline);
  const candidateWarnings = extractWarnings(params.candidate);

  const hasContextFailure = contextChecks.some((c) => c.status === "fail");
  const hasMissingRequiredMetrics = (result.comparisons ?? []).some(
    (c: any) =>
      (c.status === "missing_baseline" || c.status === "missing_candidate") && !OPTIONAL_STORAGE_METRICS.has(c.metric),
  );
  if (hasContextFailure || hasMissingRequiredMetrics) {
    result.status = "unstable";
  }

  return { result, contextChecks, baselineWarnings, candidateWarnings };
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
  const suiteThresholds = getSuiteThresholds(profile, "storage");

  // Optional backdoor overrides for local debugging / emergency CI tuning.
  // CI policy should live in `bench/perf_thresholds.json`.
  const cliThresholdPct = args.thresholdPct ? Number(args.thresholdPct) / 100 : null;
  const cliCvThreshold = args.cvThreshold ? Number(args.cvThreshold) : null;
  const envThresholdPct = process.env.STORAGE_PERF_REGRESSION_THRESHOLD_PCT
    ? Number(process.env.STORAGE_PERF_REGRESSION_THRESHOLD_PCT) / 100
    : null;
  const envExtremeCv = process.env.STORAGE_PERF_EXTREME_CV_THRESHOLD
    ? Number(process.env.STORAGE_PERF_EXTREME_CV_THRESHOLD)
    : null;

  const overrideMaxRegressionPct =
    (isFiniteNumber(cliThresholdPct) && cliThresholdPct > 0 && cliThresholdPct) ||
    (isFiniteNumber(envThresholdPct) && envThresholdPct > 0 && envThresholdPct) ||
    null;
  const overrideExtremeCv =
    (isFiniteNumber(cliCvThreshold) && cliCvThreshold > 0 && cliCvThreshold) ||
    (isFiniteNumber(envExtremeCv) && envExtremeCv > 0 && envExtremeCv) ||
    null;

  const { result, contextChecks, baselineWarnings, candidateWarnings } = buildStorageCompareResult({
    baseline,
    candidate,
    thresholdsFile,
    profileName,
    suiteThresholds,
    overrideMaxRegressionPct,
    overrideExtremeCv,
  });

  const markdown = renderStorageCompareMarkdown({
    result,
    contextChecks,
    baselineWarnings,
    candidateWarnings,
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
