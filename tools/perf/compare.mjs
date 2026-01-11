import fs from "node:fs/promises";
import path from "node:path";
import process from "node:process";

function usage(exitCode) {
  const msg = `
Usage:
  node tools/perf/compare.mjs --baseline <summary.json> --candidate <summary.json> --out-dir <dir>

Options:
  --baseline <path>                  Baseline summary.json (required)
  --candidate <path>                 Candidate summary.json (required)
  --out-dir <dir>                    Output directory (required)
  --regression-threshold-pct <n>     Fail if any benchmark regresses by >= n percent (default: 15)
  --extreme-cv-threshold <n>         Fail if any benchmark has coefficient-of-variation >= n (default: 0.5)
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
    regressionThresholdPct: 15,
    extremeCvThreshold: 0.5,
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
      case "--regression-threshold-pct":
        out.regressionThresholdPct = Number.parseFloat(argv[++i]);
        break;
      case "--extreme-cv-threshold":
        out.extremeCvThreshold = Number.parseFloat(argv[++i]);
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

function fmtPct(pct) {
  const sign = pct > 0 ? "+" : "";
  return `${sign}${(pct * 100).toFixed(2)}%`;
}

function fmtMs(ms) {
  const sign = ms > 0 ? "+" : "";
  return `${sign}${ms.toFixed(2)}ms`;
}

function mdEscape(s) {
  return String(s).replaceAll("|", "\\|");
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

  const baselineMap = new Map((baseline.benchmarks ?? []).map((b) => [b.name, b]));
  const candidateMap = new Map((candidate.benchmarks ?? []).map((b) => [b.name, b]));

  const names = [...baselineMap.keys()].filter((n) => candidateMap.has(n));
  names.sort();

  const rows = [];
  let hasRegression = false;
  let isExtremelyUnstable = false;

  for (const name of names) {
    const b = baselineMap.get(name);
    const c = candidateMap.get(name);
    const baseMedian = b?.stats?.median;
    const candMedian = c?.stats?.median;

    if (!Number.isFinite(baseMedian) || !Number.isFinite(candMedian) || baseMedian === 0) continue;

    const deltaMs = candMedian - baseMedian;
    const pct = deltaMs / baseMedian;
    const status = pct >= opts.regressionThresholdPct / 100 ? "regression" : pct <= -0.01 ? "improvement" : "ok";

    if (status === "regression") hasRegression = true;

    const baseCv = b?.stats?.cv;
    const candCv = c?.stats?.cv;
    if ((Number.isFinite(baseCv) && baseCv >= opts.extremeCvThreshold) || (Number.isFinite(candCv) && candCv >= opts.extremeCvThreshold)) {
      isExtremelyUnstable = true;
    }

    rows.push({
      name,
      baselineMedianMs: baseMedian,
      candidateMedianMs: candMedian,
      deltaMs,
      pct,
      baseCv,
      candCv,
      status,
    });
  }

  const regressions = [...rows].filter((r) => r.pct >= 0.01).sort((a, b) => b.pct - a.pct);
  const improvements = [...rows].filter((r) => r.pct <= -0.01).sort((a, b) => a.pct - b.pct);

  const status = isExtremelyUnstable ? "unstable" : hasRegression ? "fail" : "pass";

  const reportLines = [];
  reportLines.push("# Perf comparison");
  reportLines.push("");
  reportLines.push(`- Baseline: \`${baseline.meta?.gitSha ?? "unknown"}\``);
  reportLines.push(`- Candidate: \`${candidate.meta?.gitSha ?? "unknown"}\``);
  reportLines.push(`- Threshold: ${opts.regressionThresholdPct}% regression`);
  reportLines.push(`- Iterations: ${candidate.meta?.iterations ?? "?"} (median-of-N)`);
  reportLines.push(`- Baseline trace: ${fmtTraceStatus(baseline.meta)} (\`${path.join(path.dirname(opts.baseline), "trace.json")}\`)`);
  reportLines.push(`- Candidate trace: ${fmtTraceStatus(candidate.meta)} (\`${path.join(path.dirname(opts.candidate), "trace.json")}\`)`);
  reportLines.push("");
  reportLines.push("| Benchmark | Baseline (median) | Candidate (median) | Δ | Δ% | Baseline CV | Candidate CV |");
  reportLines.push("| --- | ---: | ---: | ---: | ---: | ---: | ---: |");
  for (const r of rows) {
    reportLines.push(
      `| ${mdEscape(r.name)} | ${r.baselineMedianMs.toFixed(2)}ms | ${r.candidateMedianMs.toFixed(2)}ms | ${fmtMs(r.deltaMs)} | ${fmtPct(r.pct)} | ${Number.isFinite(r.baseCv) ? r.baseCv.toFixed(2) : "n/a"} | ${Number.isFinite(r.candCv) ? r.candCv.toFixed(2) : "n/a"} |`,
    );
  }

  reportLines.push("");
  if (status === "unstable") {
    reportLines.push("Result: **Unstable** (extreme variance detected; see raw samples in artifacts)");
  } else if (status === "fail") {
    reportLines.push("Result: **Regression detected**");
  } else {
    reportLines.push("Result: **No significant regressions**");
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

  const summary = {
    status,
    regressionThresholdPct: opts.regressionThresholdPct,
    extremeCvThreshold: opts.extremeCvThreshold,
    iterations: candidate.meta?.iterations,
    baseline: { gitSha: baseline.meta?.gitSha },
    candidate: { gitSha: candidate.meta?.gitSha },
    benchmarks: rows.map((r) => ({
      name: r.name,
      deltaMs: r.deltaMs,
      pct: r.pct,
      baselineMedianMs: r.baselineMedianMs,
      candidateMedianMs: r.candidateMedianMs,
      baselineCv: r.baseCv,
      candidateCv: r.candCv,
    })),
    topRegressions: regressions.slice(0, 5).map((r) => ({ name: r.name, deltaMs: r.deltaMs, pct: r.pct })),
    topImprovements: improvements.slice(0, 5).map((r) => ({ name: r.name, deltaMs: r.deltaMs, pct: r.pct })),
  };

  await Promise.all([
    fs.writeFile(path.join(outDir, "compare.md"), reportLines.join("\n")),
    fs.writeFile(path.join(outDir, "summary.json"), JSON.stringify(summary, null, 2)),
  ]);

  if (status === "unstable") {
    process.exitCode = 2;
  } else if (status === "fail") {
    process.exitCode = 1;
  } else {
    process.exitCode = 0;
  }
}

await main();
