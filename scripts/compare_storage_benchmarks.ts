/**
 * Compatibility wrapper for storage perf comparisons.
 *
 * Canonical compare implementation:
 *   node --experimental-strip-types bench/compare.ts ...
 *
 * This wrapper keeps older invocations working (e.g. `--current`, `--outDir`,
 * `--thresholdPct`, `--json`) while delegating to `bench/compare.ts` so:
 * - thresholds come from `bench/perf_thresholds.json` (or `--thresholds-file`)
 * - exit codes match the shared convention (0 pass, 1 regression, 2 unstable)
 * - output artifacts are consistent (`compare.md` + `summary.json`)
 */

import { spawnSync } from "node:child_process";
import fs from "node:fs/promises";
import path from "node:path";
import process from "node:process";

function parseArgs(argv: string[]): Record<string, string> {
  const out: Record<string, string> = {};
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

function usage(exitCode: number): never {
  const msg = `
Usage:
  node --experimental-strip-types scripts/compare_storage_benchmarks.ts --baseline <storage_bench.json> --current <storage_bench.json>

This is a compatibility wrapper around \`bench/compare.ts\`.

Options:
  --baseline <path>          Baseline storage_bench.json (required)
  --current <path>           Current storage_bench.json (required; alias: --candidate)
  --candidate <path>         Alias for --current
  --outDir <dir>             Output dir (default: storage-perf-results; alias: --out-dir)
  --out-dir <dir>            Alias for --outDir
  --thresholds-file <path>   Threshold policy (default: bench/perf_thresholds.json)
  --profile <name>           Threshold profile (default: pr-smoke)
  --help, -h                 Show this help

Legacy override flags (optional):
  --thresholdPct <n>         Override maxRegressionPct for all storage metrics (percent)
  --cvThreshold <n>          Override extremeCvThreshold for all storage metrics

Legacy JSON output (optional):
  --json                     Write a copy of summary.json to <outDir>/compare.json
  --outputJson <path>        Write a copy of summary.json to <path>
  --outputMd <path>          Write a copy of compare.md to <path>

Environment overrides (optional):
  STORAGE_PERF_REGRESSION_THRESHOLD_PCT=15
  STORAGE_PERF_EXTREME_CV_THRESHOLD=0.5
`.trim();
  console.log(msg);
  process.exit(exitCode);
}

async function copyFileIfPresent(src: string, dest: string) {
  try {
    await fs.mkdir(path.dirname(dest), { recursive: true });
    await fs.copyFile(src, dest);
  } catch (err: any) {
    if (err?.code === "ENOENT") return;
    throw err;
  }
}

async function main() {
  const rawArgs = process.argv.slice(2);
  if (rawArgs.includes("-h")) usage(0);

  const args = parseArgs(rawArgs);
  if (args.help !== undefined) usage(0);

  const baselinePath = args.baseline;
  const candidatePath = args.current ?? args.candidate;
  const outDir = args["out-dir"] ?? args.outDir ?? "storage-perf-results";
  const thresholdsFile = args["thresholds-file"] ?? args.thresholdsFile;
  const profile = args.profile;

  if (!baselinePath || !candidatePath) {
    console.error("Missing required args: --baseline <path> --current <path> (or --candidate <path>)");
    usage(1);
  }

  const env = { ...process.env };
  if (args.thresholdPct) env.STORAGE_PERF_REGRESSION_THRESHOLD_PCT = String(args.thresholdPct);
  if (args.cvThreshold) env.STORAGE_PERF_EXTREME_CV_THRESHOLD = String(args.cvThreshold);

  const childArgs = [
    "--experimental-strip-types",
    "bench/compare.ts",
    "--baseline",
    baselinePath,
    "--candidate",
    candidatePath,
    "--out-dir",
    outDir,
  ];
  if (thresholdsFile) childArgs.push("--thresholds-file", thresholdsFile);
  if (profile) childArgs.push("--profile", profile);

  const res = spawnSync(process.execPath, childArgs, {
    cwd: process.cwd(),
    env,
    encoding: "utf8",
    stdio: "inherit",
  });

  const exitCode = typeof res.status === "number" ? res.status : 1;
  process.exitCode = exitCode;

  // Optional legacy outputs: copy artifacts written by bench/compare.ts.
  const resolvedOutDir = path.resolve(process.cwd(), outDir);
  const compareMdPath = path.join(resolvedOutDir, "compare.md");
  const summaryPath = path.join(resolvedOutDir, "summary.json");

  let outputJson: string | null = null;
  if (args.outputJson) outputJson = args.outputJson;
  if (args["output-json"]) outputJson = args["output-json"];
  if (args.json === "true" && !outputJson) outputJson = path.join(resolvedOutDir, "compare.json");
  if (outputJson) {
    const resolvedOut = path.resolve(process.cwd(), outputJson);
    await copyFileIfPresent(summaryPath, resolvedOut);
  }

  let outputMd: string | null = null;
  if (args.outputMd) outputMd = args.outputMd;
  if (args["output-md"]) outputMd = args["output-md"];
  if (outputMd) {
    const resolvedOut = path.resolve(process.cwd(), outputMd);
    await copyFileIfPresent(compareMdPath, resolvedOut);
  }
}

if (import.meta.url === `file://${process.argv[1]}`) {
  main().catch((err) => {
    console.error(err?.stack ?? String(err));
    process.exitCode = 1;
  });
}

