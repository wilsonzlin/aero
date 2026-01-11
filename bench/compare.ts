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

Backdoor overrides (optional):
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

async function readJson(file: string): Promise<any> {
  return JSON.parse(await fs.readFile(file, "utf8"));
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

async function main() {
  const args = parseArgs(process.argv.slice(2));
  if (args.help === "true") usage(0);

  const baselinePath = args.baseline;
  const candidatePath = args.candidate;
  const outDir = args["out-dir"];
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

  /** @type {any[]} */
  const cases = [];

  for (const [metricName, threshold] of Object.entries(suiteThresholds.metrics ?? {})) {
    const better = (threshold as any)?.better;
    if (better !== "lower" && better !== "higher") {
      throw new Error(`thresholds: storage.metrics.${metricName}.better must be "lower" or "higher"`);
    }

    let baselineSamples: number[] | null = null;
    let candidateSamples: number[] | null = null;
    let unit = "";

    switch (metricName) {
      case "sequential_write_mb_per_s":
        baselineSamples = throughputSamples(baseline, "sequential_write");
        candidateSamples = throughputSamples(candidate, "sequential_write");
        unit = "MB/s";
        break;
      case "sequential_read_mb_per_s":
        baselineSamples = throughputSamples(baseline, "sequential_read");
        candidateSamples = throughputSamples(candidate, "sequential_read");
        unit = "MB/s";
        break;
      case "random_read_4k_p95_ms":
        baselineSamples = latencyP95Samples(baseline, "random_read_4k");
        candidateSamples = latencyP95Samples(candidate, "random_read_4k");
        unit = "ms";
        break;
      case "random_write_4k_p95_ms":
        baselineSamples = latencyP95Samples(baseline, "random_write_4k");
        candidateSamples = latencyP95Samples(candidate, "random_write_4k");
        unit = "ms";
        break;
      default:
        // Unknown metric in policy; include as missing so it appears in the report.
        baselineSamples = null;
        candidateSamples = null;
        unit = "";
        break;
    }

    cases.push({
      scenario: "storage",
      metric: metricName,
      unit,
      better,
      threshold,
      baseline: baselineSamples ? statsFromSamples(baselineSamples) : null,
      candidate: candidateSamples ? statsFromSamples(candidateSamples) : null,
    });
  }

  const envMaxRegressionPct = process.env.STORAGE_PERF_REGRESSION_THRESHOLD_PCT
    ? Number(process.env.STORAGE_PERF_REGRESSION_THRESHOLD_PCT) / 100
    : null;
  const envExtremeCv = process.env.STORAGE_PERF_EXTREME_CV_THRESHOLD
    ? Number(process.env.STORAGE_PERF_EXTREME_CV_THRESHOLD)
    : null;

  if (isFiniteNumber(envMaxRegressionPct) || isFiniteNumber(envExtremeCv)) {
    for (const c of cases) {
      if (isFiniteNumber(envMaxRegressionPct)) c.threshold = { ...c.threshold, maxRegressionPct: envMaxRegressionPct };
      if (isFiniteNumber(envExtremeCv)) c.threshold = { ...c.threshold, extremeCvThreshold: envExtremeCv };
    }
  }

  const result = buildCompareResult({
    suite: "storage",
    profile: profileName,
    thresholdsFile,
    baselineMeta: { run_id: baseline?.run_id, backend: baseline?.backend, api_mode: baseline?.api_mode },
    candidateMeta: { run_id: candidate?.run_id, backend: candidate?.backend, api_mode: candidate?.api_mode },
    cases,
  });

  const markdown = renderCompareMarkdown(result, { title: "Storage perf comparison" });
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
