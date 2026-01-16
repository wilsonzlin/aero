import fs from "node:fs/promises";
import path from "node:path";
import process from "node:process";
import { buildCompareResult, exitCodeForStatus, renderCompareMarkdown } from "./lib/compare_core.mjs";
import {
  DEFAULT_PROFILE,
  DEFAULT_THRESHOLDS_FILE,
  getSuiteThresholds,
  loadThresholdPolicy,
  pickThresholdProfile,
} from "./lib/thresholds.mjs";

function usage(exitCode) {
  const msg = `
Usage:
  node tools/perf/compare.mjs --baseline <summary.json> --candidate <summary.json> --out-dir <dir>

Options:
  --baseline <path>                  Baseline summary.json (required)
  --candidate <path>                 Candidate summary.json (required)
  --out-dir <dir>                    Output directory (required)
  --thresholds-file <path>           Threshold policy file (default: ${DEFAULT_THRESHOLDS_FILE})
  --profile <pr-smoke|nightly>       Threshold profile (default: ${DEFAULT_PROFILE})
  --regression-threshold-pct <pct>   Override maxRegressionPct for all browser metrics (percent, e.g. 15)
  --extreme-cv-threshold <cv>        Override extremeCvThreshold for all browser metrics (e.g. 0.5)
  --help                             Show this help
 `;
  console.log(msg.trim());
  process.exit(exitCode);
}

function parseArgs(argv) {
  const out = {
    baseline: undefined,
    candidate: undefined,
    outDir: undefined,
    thresholdsFile: DEFAULT_THRESHOLDS_FILE,
    profile: DEFAULT_PROFILE,
    regressionThresholdPct: undefined,
    extremeCvThreshold: undefined,
  };

  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    switch (arg) {
      case "--baseline":
        out.baseline = argv[++i];
        break;
      case "--candidate":
        out.candidate = argv[++i];
        break;
      case "--out-dir":
        out.outDir = argv[++i];
        break;
      case "--thresholds-file":
        out.thresholdsFile = argv[++i];
        break;
      case "--profile":
        out.profile = argv[++i];
        break;
      case "--regression-threshold-pct":
        out.regressionThresholdPct = argv[++i];
        break;
      case "--extreme-cv-threshold":
        out.extremeCvThreshold = argv[++i];
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

  if (!out.baseline || !out.candidate || !out.outDir) {
    console.error("--baseline, --candidate, and --out-dir are required");
    usage(1);
  }

  return out;
}

function mdEscape(s) {
  return coerceScalarString(s).replaceAll("|", "\\|");
}

function coerceScalarString(value) {
  if (value == null) return "";
  switch (typeof value) {
    case "string":
      return value;
    case "number":
    case "boolean":
    case "bigint":
      return String(value);
    case "symbol":
    case "undefined":
    case "object":
    case "function":
    default:
      return "";
  }
}

function fmtCount(n) {
  if (!Number.isFinite(n)) return "n/a";
  return Math.round(n).toString();
}

function fmtBytes(bytes) {
  if (!Number.isFinite(bytes)) return "n/a";
  const abs = Math.abs(bytes);
  if (abs < 1024) return `${bytes.toFixed(0)} B`;
  if (abs < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  if (abs < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(1)} GB`;
}

function fmtSignedNumber(n, decimals) {
  if (!Number.isFinite(n)) return "n/a";
  const sign = n > 0 ? "+" : "";
  return `${sign}${n.toFixed(decimals)}`;
}

function fmtSignedBytes(bytes) {
  if (!Number.isFinite(bytes)) return "n/a";
  const sign = bytes > 0 ? "+" : "";
  return `${sign}${fmtBytes(bytes)}`;
}

function fmtSignedCount(n) {
  if (!Number.isFinite(n)) return "n/a";
  const sign = n > 0 ? "+" : "";
  return `${sign}${Math.round(n)}`;
}

async function readJson(file) {
  return JSON.parse(await fs.readFile(file, "utf8"));
}

function isFiniteNumber(n) {
  return typeof n === "number" && Number.isFinite(n);
}

async function main() {
  const opts = parseArgs(process.argv.slice(2));
  const outDir = path.resolve(process.cwd(), opts.outDir);
  await fs.mkdir(outDir, { recursive: true });

  const baseline = await readJson(opts.baseline);
  const candidate = await readJson(opts.candidate);

  const fmtTraceStatus = (meta) => {
    const trace = meta?.aeroPerf?.trace;
    if (!trace || typeof trace !== "object") return "unknown";
    const requested = trace.requested === true;
    const available = trace.available === true;
    const captured = trace.captured === true;
    const timedOut = trace.timedOut === true;
    if (!requested) return available ? "available (not captured)" : "not captured";
    if (timedOut) return "timed out";
    if (!available) return "unsupported";
    return captured ? "captured" : "not captured";
  };

  const fmtPerfExportStatus = (meta) => {
    const aeroPerf = meta?.aeroPerf;
    if (!aeroPerf || typeof aeroPerf !== "object") return "unknown";
    const available = aeroPerf.exportAvailable === true;
    const timedOut = aeroPerf.exportApiTimedOut === true;
    if (available) return timedOut ? "available (late)" : "available";
    return timedOut ? "timed out" : "unavailable";
  };

  const thresholdsPolicy = await loadThresholdPolicy(opts.thresholdsFile);
  const { name: profileName, profile } = pickThresholdProfile(thresholdsPolicy, opts.profile);
  const suiteThresholds = getSuiteThresholds(profile, "browser");

  const baselineMap = new Map((baseline.benchmarks ?? []).map((b) => [b.name, b]));
  const candidateMap = new Map((candidate.benchmarks ?? []).map((b) => [b.name, b]));

  const cases = [];
  for (const [metricName, threshold] of Object.entries(suiteThresholds.metrics ?? {})) {
    const better = threshold?.better;
    if (better !== "lower" && better !== "higher") {
      throw new Error(`thresholds: browser.metrics.${metricName}.better must be "lower" or "higher"`);
    }

    const b = baselineMap.get(metricName);
    const c = candidateMap.get(metricName);
    const unitRaw = b?.unit ?? c?.unit ?? "";
    const unit = coerceScalarString(unitRaw).slice(0, 64);

    const baselineStats = b?.stats
      ? {
          value: b.stats.median,
          cv: b.stats.cv,
          n: b.stats.n,
        }
      : null;
    const candidateStats = c?.stats
      ? {
          value: c.stats.median,
          cv: c.stats.cv,
          n: c.stats.n,
        }
      : null;

    cases.push({
      scenario: "browser",
      metric: metricName,
      unit,
      better,
      threshold,
      baseline: baselineStats,
      candidate: candidateStats,
    });
  }

  // Optional backdoor: allow overriding *all* browser thresholds via env vars.
  // This keeps CI policy centralized in `bench/perf_thresholds.json`, while still
  // allowing one-off local/CI debugging without editing policy.
  const envMaxRegressionPct = process.env.PERF_REGRESSION_THRESHOLD_PCT
    ? Number(process.env.PERF_REGRESSION_THRESHOLD_PCT) / 100
    : null;
  const envExtremeCv = process.env.PERF_EXTREME_CV_THRESHOLD ? Number(process.env.PERF_EXTREME_CV_THRESHOLD) : null;

  const cliMaxRegressionPct =
    typeof opts.regressionThresholdPct === "string" ? Number(opts.regressionThresholdPct) / 100 : null;
  const cliExtremeCv = typeof opts.extremeCvThreshold === "string" ? Number(opts.extremeCvThreshold) : null;

  if (opts.regressionThresholdPct !== undefined && !isFiniteNumber(cliMaxRegressionPct)) {
    console.error(
      `Invalid --regression-threshold-pct: expected a finite number, got ${JSON.stringify(opts.regressionThresholdPct)}`,
    );
    usage(1);
  }

  if (opts.extremeCvThreshold !== undefined && !isFiniteNumber(cliExtremeCv)) {
    console.error(
      `Invalid --extreme-cv-threshold: expected a finite number, got ${JSON.stringify(opts.extremeCvThreshold)}`,
    );
    usage(1);
  }

  // Precedence: env overrides thresholds file, CLI overrides env.
  const overrideMaxRegressionPct = isFiniteNumber(cliMaxRegressionPct)
    ? cliMaxRegressionPct
    : isFiniteNumber(envMaxRegressionPct)
      ? envMaxRegressionPct
      : null;
  const overrideExtremeCv = isFiniteNumber(cliExtremeCv)
    ? cliExtremeCv
    : isFiniteNumber(envExtremeCv)
      ? envExtremeCv
      : null;

  if (overrideMaxRegressionPct !== null || overrideExtremeCv !== null) {
    for (const c of cases) {
      if (overrideMaxRegressionPct !== null) {
        c.threshold = { ...c.threshold, maxRegressionPct: overrideMaxRegressionPct };
      }
      if (overrideExtremeCv !== null) {
        c.threshold = { ...c.threshold, extremeCvThreshold: overrideExtremeCv };
      }
    }
  }

  const result = buildCompareResult({
    suite: "browser",
    profile: profileName,
    thresholdsFile: opts.thresholdsFile,
    baselineMeta: baseline.meta ?? null,
    candidateMeta: candidate.meta ?? null,
    cases,
  });

  const reportLines = renderCompareMarkdown(result, { title: "Perf comparison" }).trimEnd().split("\n");
  const summaryIdx = reportLines.findIndex((line) => line.startsWith("## Summary"));
  if (summaryIdx !== -1) {
    reportLines.splice(summaryIdx - 1, 0, [
      `- Baseline perf export: ${fmtPerfExportStatus(baseline.meta)} (\`${path.join(path.dirname(opts.baseline), "perf_export.json")}\`)`,
      `- Candidate perf export: ${fmtPerfExportStatus(candidate.meta)} (\`${path.join(path.dirname(opts.candidate), "perf_export.json")}\`)`,
      `- Baseline trace: ${fmtTraceStatus(baseline.meta)} (\`${path.join(path.dirname(opts.baseline), "trace.json")}\`)`,
      `- Candidate trace: ${fmtTraceStatus(candidate.meta)} (\`${path.join(path.dirname(opts.candidate), "trace.json")}\`)`,
    ].join("\n"));
  }

  const baseJit = baseline.meta?.aeroPerf?.jit;
  const candJit = candidate.meta?.aeroPerf?.jit;
  if (baseJit !== undefined || candJit !== undefined) {
    const asNum = (v) => (typeof v === "number" && Number.isFinite(v) ? v : null);
    const asBool = (v) => (typeof v === "boolean" ? v : null);

      const summarize = (jit) => {
        if (!jit || typeof jit !== "object") return null;
        const tier1 = jit.totals?.tier1;
        const tier2 = jit.totals?.tier2;
        const cache = jit.totals?.cache;
        const deopt = jit.totals?.deopt;
        const passes = tier2?.passesMs;
        return {
          enabled: asBool(jit.enabled),
          t1Blocks: asNum(tier1?.blocksCompiled),
          t2Blocks: asNum(tier2?.blocksCompiled),
          t1CompileMs: asNum(tier1?.compileMs),
          t2CompileMs: asNum(tier2?.compileMs),
          t2ConstFoldMs: asNum(passes?.constFold),
          t2DceMs: asNum(passes?.dce),
          t2RegallocMs: asNum(passes?.regalloc),
          cacheHits: asNum(cache?.lookupHit),
          cacheMisses: asNum(cache?.lookupMiss),
          cacheInstalls: asNum(cache?.install),
          cacheEvicts: asNum(cache?.evict),
          cacheInvalidates: asNum(cache?.invalidate),
          cacheStaleInstallRejects: asNum(cache?.staleInstallReject),
          compileRequests: asNum(cache?.compileRequest),
          cacheUsedBytes: asNum(cache?.usedBytes),
          cacheCapacityBytes: asNum(cache?.capacityBytes),
          deopt: asNum(deopt?.count),
          guardFail: asNum(deopt?.guardFail),
        };
      };

    const base = summarize(baseJit);
    const cand = summarize(candJit);

    const cacheHitRate = (s) => {
      if (!s) return null;
      if (s.cacheHits == null || s.cacheMisses == null) return null;
      const total = s.cacheHits + s.cacheMisses;
      if (total <= 0) return 0;
      return s.cacheHits / total;
    };

    reportLines.push("");
    reportLines.push("## Aero JIT metrics (PF-006)");
    reportLines.push("");
    reportLines.push("| Metric | Baseline | Candidate | Δ |");
    reportLines.push("| --- | ---: | ---: | ---: |");

    const addRow = (label, b, c, opts) => {
      const { fmt, fmtDelta } = opts;
      const bText = b == null ? "n/a" : fmt(b);
      const cText = c == null ? "n/a" : fmt(c);
      const deltaText = b == null || c == null ? "n/a" : fmtDelta(c - b);
      reportLines.push(`| ${mdEscape(label)} | ${bText} | ${cText} | ${deltaText} |`);
    };

    // Enabled: show baseline/candidate, no delta.
    reportLines.push(
      `| enabled | ${base?.enabled == null ? "n/a" : base.enabled ? "true" : "false"} | ${
        cand?.enabled == null ? "n/a" : cand.enabled ? "true" : "false"
      } | — |`,
    );

    addRow("tier1 blocks compiled", base?.t1Blocks, cand?.t1Blocks, {
      fmt: fmtCount,
      fmtDelta: fmtSignedCount,
    });
    addRow("tier2 blocks compiled", base?.t2Blocks, cand?.t2Blocks, {
      fmt: fmtCount,
      fmtDelta: fmtSignedCount,
    });
    addRow("tier1 compile ms (total)", base?.t1CompileMs, cand?.t1CompileMs, {
      fmt: (n) => `${n.toFixed(2)}ms`,
      fmtDelta: (n) => `${fmtSignedNumber(n, 2)}ms`,
    });
    addRow("tier2 compile ms (total)", base?.t2CompileMs, cand?.t2CompileMs, {
      fmt: (n) => `${n.toFixed(2)}ms`,
      fmtDelta: (n) => `${fmtSignedNumber(n, 2)}ms`,
    });
    addRow("tier2 pass ms: const-fold", base?.t2ConstFoldMs, cand?.t2ConstFoldMs, {
      fmt: (n) => `${n.toFixed(2)}ms`,
      fmtDelta: (n) => `${fmtSignedNumber(n, 2)}ms`,
    });
    addRow("tier2 pass ms: DCE", base?.t2DceMs, cand?.t2DceMs, {
      fmt: (n) => `${n.toFixed(2)}ms`,
      fmtDelta: (n) => `${fmtSignedNumber(n, 2)}ms`,
    });
    addRow("tier2 pass ms: regalloc", base?.t2RegallocMs, cand?.t2RegallocMs, {
      fmt: (n) => `${n.toFixed(2)}ms`,
      fmtDelta: (n) => `${fmtSignedNumber(n, 2)}ms`,
    });
    addRow("cache lookups (hit)", base?.cacheHits, cand?.cacheHits, {
      fmt: fmtCount,
      fmtDelta: fmtSignedCount,
    });
    addRow("cache lookups (miss)", base?.cacheMisses, cand?.cacheMisses, {
      fmt: fmtCount,
      fmtDelta: fmtSignedCount,
    });
    addRow("cache installs", base?.cacheInstalls, cand?.cacheInstalls, {
      fmt: fmtCount,
      fmtDelta: fmtSignedCount,
    });
    addRow("cache evictions", base?.cacheEvicts, cand?.cacheEvicts, {
      fmt: fmtCount,
      fmtDelta: fmtSignedCount,
    });
    addRow("cache invalidations", base?.cacheInvalidates, cand?.cacheInvalidates, {
      fmt: fmtCount,
      fmtDelta: fmtSignedCount,
    });
    addRow("cache stale install rejects", base?.cacheStaleInstallRejects, cand?.cacheStaleInstallRejects, {
      fmt: fmtCount,
      fmtDelta: fmtSignedCount,
    });
    addRow("compile requests", base?.compileRequests, cand?.compileRequests, {
      fmt: fmtCount,
      fmtDelta: fmtSignedCount,
    });
    addRow("cache hit rate (total)", cacheHitRate(base), cacheHitRate(cand), {
      fmt: (n) => `${(n * 100).toFixed(1)}%`,
      fmtDelta: (n) => `${fmtSignedNumber(n * 100, 1)}%`,
    });
    addRow("code cache used", base?.cacheUsedBytes, cand?.cacheUsedBytes, {
      fmt: fmtBytes,
      fmtDelta: fmtSignedBytes,
    });
    addRow("code cache capacity", base?.cacheCapacityBytes, cand?.cacheCapacityBytes, {
      fmt: fmtBytes,
      fmtDelta: fmtSignedBytes,
    });
    addRow("deopt count", base?.deopt, cand?.deopt, { fmt: fmtCount, fmtDelta: fmtSignedCount });
    addRow("guard fail count", base?.guardFail, cand?.guardFail, { fmt: fmtCount, fmtDelta: fmtSignedCount });
  }

  await Promise.all([
    fs.writeFile(path.join(outDir, "compare.md"), reportLines.join("\n")),
    fs.writeFile(path.join(outDir, "summary.json"), JSON.stringify(result, null, 2)),
  ]);

  process.exitCode = exitCodeForStatus(result.status);
}

await main();
