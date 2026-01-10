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

async function readJson(file) {
  return JSON.parse(await fs.readFile(file, "utf8"));
}

async function main() {
  const opts = parseArgs(process.argv.slice(2));
  const outDir = path.resolve(process.cwd(), opts.outDir);
  await fs.mkdir(outDir, { recursive: true });

  const baseline = await readJson(opts.baseline);
  const candidate = await readJson(opts.candidate);

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
