import test from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";

const writeJson = async (filePath, value) => {
  await writeFile(filePath, JSON.stringify(value, null, 2));
};

const runCompare = ({ baseline, candidate, thresholdsFile, profile = "pr-smoke", env = {} }) =>
  spawnSync(
    process.execPath,
    [
      "tools/perf/compare.mjs",
      "--baseline",
      baseline,
      "--candidate",
      candidate,
      "--out-dir",
      path.join(path.dirname(baseline), "out"),
      "--thresholds-file",
      thresholdsFile,
      "--profile",
      profile,
    ],
    { encoding: "utf8", env: { ...process.env, ...env } },
  );

test("tools/perf/compare.mjs fails on regression above threshold", async () => {
  const dir = await mkdtemp(path.join(os.tmpdir(), "aero-perf-compare-"));
  try {
    const baselinePath = path.join(dir, "baseline.json");
    const candidatePath = path.join(dir, "candidate.json");

    await writeJson(baselinePath, {
      meta: { gitSha: "base", iterations: 3 },
      benchmarks: [
        { name: "chromium_startup_ms", unit: "ms", stats: { median: 100, cv: 0.1 } },
        { name: "microbench_ms", unit: "ms", stats: { median: 50, cv: 0.1 } },
      ],
    });

    await writeJson(candidatePath, {
      meta: { gitSha: "head", iterations: 3 },
      benchmarks: [
        { name: "chromium_startup_ms", unit: "ms", stats: { median: 120, cv: 0.1 } },
        { name: "microbench_ms", unit: "ms", stats: { median: 49, cv: 0.1 } },
      ],
    });

    const thresholdsPath = path.join(dir, "thresholds.json");
    await writeJson(thresholdsPath, {
      schemaVersion: 1,
      profiles: {
        "pr-smoke": {
          browser: {
            metrics: {
              chromium_startup_ms: { better: "lower", maxRegressionPct: 0.15, extremeCvThreshold: 0.5 },
              microbench_ms: { better: "lower", maxRegressionPct: 0.15, extremeCvThreshold: 0.5 },
            },
          },
        },
      },
    });

    const result = runCompare({ baseline: baselinePath, candidate: candidatePath, thresholdsFile: thresholdsPath });
    assert.equal(result.status, 1, `expected exit code 1, got ${result.status}\n${result.stderr}`);

    const outSummary = JSON.parse(await readFile(path.join(dir, "out", "summary.json"), "utf8"));
    assert.equal(outSummary.status, "regression");
    assert.equal(outSummary.topRegressions[0]?.metric, "chromium_startup_ms");

    const compareMd = await readFile(path.join(dir, "out", "compare.md"), "utf8");
    assert.ok(compareMd.includes("# Perf comparison"));
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test("tools/perf/compare.mjs passes when within threshold", async () => {
  const dir = await mkdtemp(path.join(os.tmpdir(), "aero-perf-compare-"));
  try {
    const baselinePath = path.join(dir, "baseline.json");
    const candidatePath = path.join(dir, "candidate.json");
    const thresholdsPath = path.join(dir, "thresholds.json");

    await writeJson(baselinePath, {
      meta: { gitSha: "base", iterations: 3 },
      benchmarks: [
        { name: "chromium_startup_ms", unit: "ms", stats: { median: 100, cv: 0.1 } },
        { name: "microbench_ms", unit: "ms", stats: { median: 50, cv: 0.1 } },
      ],
    });

    await writeJson(candidatePath, {
      meta: { gitSha: "head", iterations: 3 },
      benchmarks: [
        { name: "chromium_startup_ms", unit: "ms", stats: { median: 110, cv: 0.1 } },
        { name: "microbench_ms", unit: "ms", stats: { median: 50, cv: 0.1 } },
      ],
    });

    await writeJson(thresholdsPath, {
      schemaVersion: 1,
      profiles: {
        "pr-smoke": {
          browser: {
            metrics: {
              chromium_startup_ms: { better: "lower", maxRegressionPct: 0.15, extremeCvThreshold: 0.5 },
              microbench_ms: { better: "lower", maxRegressionPct: 0.15, extremeCvThreshold: 0.5 },
            },
          },
        },
      },
    });

    const result = runCompare({ baseline: baselinePath, candidate: candidatePath, thresholdsFile: thresholdsPath });
    assert.equal(result.status, 0, `expected exit code 0, got ${result.status}\n${result.stderr}`);

    const outSummary = JSON.parse(await readFile(path.join(dir, "out", "summary.json"), "utf8"));
    assert.equal(outSummary.status, "pass");
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test("tools/perf/compare.mjs returns unstable on extreme coefficient-of-variation", async () => {
  const dir = await mkdtemp(path.join(os.tmpdir(), "aero-perf-compare-"));
  try {
    const baselinePath = path.join(dir, "baseline.json");
    const candidatePath = path.join(dir, "candidate.json");
    const thresholdsPath = path.join(dir, "thresholds.json");

    await writeJson(baselinePath, {
      meta: { gitSha: "base", iterations: 3 },
      benchmarks: [
        { name: "chromium_startup_ms", unit: "ms", stats: { median: 100, cv: 0.1 } },
        { name: "microbench_ms", unit: "ms", stats: { median: 50, cv: 0.1 } },
      ],
    });

    await writeJson(candidatePath, {
      meta: { gitSha: "head", iterations: 3 },
      benchmarks: [
        { name: "chromium_startup_ms", unit: "ms", stats: { median: 100, cv: 0.9 } },
        { name: "microbench_ms", unit: "ms", stats: { median: 50, cv: 0.1 } },
      ],
    });

    await writeJson(thresholdsPath, {
      schemaVersion: 1,
      profiles: {
        "pr-smoke": {
          browser: {
            metrics: {
              chromium_startup_ms: { better: "lower", maxRegressionPct: 0.15, extremeCvThreshold: 0.5 },
              microbench_ms: { better: "lower", maxRegressionPct: 0.15, extremeCvThreshold: 0.5 },
            },
          },
        },
      },
    });

    const result = runCompare({ baseline: baselinePath, candidate: candidatePath, thresholdsFile: thresholdsPath });
    assert.equal(result.status, 2, `expected exit code 2, got ${result.status}\n${result.stderr}`);

    const outSummary = JSON.parse(await readFile(path.join(dir, "out", "summary.json"), "utf8"));
    assert.equal(outSummary.status, "unstable");
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});
