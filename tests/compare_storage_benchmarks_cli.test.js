import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";

function writeJson(filePath, value) {
  fs.writeFileSync(filePath, `${JSON.stringify(value, null, 2)}\n`, "utf8");
}

function runCompare(args, opts = {}) {
  const result = spawnSync(
    process.execPath,
    [
      "--experimental-strip-types",
      "--import",
      "./scripts/register-ts-strip-loader.mjs",
      "scripts/compare_storage_benchmarks.ts",
      ...args,
    ],
    {
      cwd: path.resolve("."),
      env: { ...process.env, ...(opts.env ?? {}) },
      encoding: "utf8",
    },
  );
  return result;
}

function makeStorageBench(overrides = {}) {
  const throughputSummary = (mbPerS) => ({
    runs: [
      { bytes: 1024 * 1024, duration_ms: 10, mb_per_s: mbPerS },
      { bytes: 1024 * 1024, duration_ms: 10, mb_per_s: mbPerS },
    ],
    mean_mb_per_s: mbPerS,
    stdev_mb_per_s: 0,
  });

  const latencySummary = (p95Ms) => ({
    runs: [
      {
        ops: 1,
        block_bytes: 4096,
        min_ms: p95Ms,
        max_ms: p95Ms,
        mean_ms: p95Ms,
        stdev_ms: 0,
        p50_ms: p95Ms,
        p95_ms: p95Ms,
      },
      {
        ops: 1,
        block_bytes: 4096,
        min_ms: p95Ms,
        max_ms: p95Ms,
        mean_ms: p95Ms,
        stdev_ms: 0,
        p50_ms: p95Ms,
        p95_ms: p95Ms,
      },
    ],
    mean_p50_ms: p95Ms,
    mean_p95_ms: p95Ms,
    stdev_p50_ms: 0,
    stdev_p95_ms: 0,
  });

  return {
    version: 1,
    run_id: "test-run",
    backend: "opfs",
    api_mode: "async",
    config: {
      seq_total_mb: 32,
      seq_chunk_mb: 4,
      seq_runs: 2,
      warmup_mb: 8,
      random_ops: 500,
      random_runs: 2,
      random_space_mb: 4,
      random_seed: 1337,
      include_random_write: false,
    },
    sequential_write: throughputSummary(100),
    sequential_read: throughputSummary(150),
    random_read_4k: latencySummary(10),
    ...overrides,
  };
}

function makeThresholdPolicy(overrides = {}) {
  return {
    schemaVersion: 1,
    profiles: {
      "pr-smoke": {
        storage: {
          metrics: {
            sequential_write_mb_per_s: { better: "higher", maxRegressionPct: 0.15, extremeCvThreshold: 0.5 },
            sequential_read_mb_per_s: { better: "higher", maxRegressionPct: 0.15, extremeCvThreshold: 0.5 },
            random_read_4k_p95_ms: { better: "lower", maxRegressionPct: 0.15, extremeCvThreshold: 0.5 },
            random_write_4k_p95_ms: { better: "lower", maxRegressionPct: 0.15, informational: true },
          },
        },
      },
    },
    ...overrides,
  };
}

test("compare_storage_benchmarks CLI supports --candidate + --out-dir and writes artifacts", () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "aero-storage-compare-"));
  const baselinePath = path.join(tmpDir, "baseline.json");
  const candidatePath = path.join(tmpDir, "candidate.json");
  const thresholdsPath = path.join(tmpDir, "thresholds.json");
  const outDir = path.join(tmpDir, "out");

  writeJson(baselinePath, makeStorageBench());

  const candidate = makeStorageBench();
  candidate.sequential_write = {
    ...candidate.sequential_write,
    mean_mb_per_s: 101,
    runs: [
      { bytes: 1, duration_ms: 1, mb_per_s: 101 },
      { bytes: 1, duration_ms: 1, mb_per_s: 101 },
    ],
  };
  writeJson(candidatePath, candidate);
  writeJson(thresholdsPath, makeThresholdPolicy());

  const res = runCompare([
    "--baseline",
    baselinePath,
    "--candidate",
    candidatePath,
    "--out-dir",
    outDir,
    "--thresholds-file",
    thresholdsPath,
    "--profile",
    "pr-smoke",
    "--json",
  ]);

  assert.equal(res.status, 0, `expected exit=0, got ${res.status}\nstdout:\n${res.stdout}\nstderr:\n${res.stderr}`);
  assert.ok(fs.existsSync(path.join(outDir, "compare.md")));
  assert.ok(fs.existsSync(path.join(outDir, "summary.json")));
  assert.ok(fs.existsSync(path.join(outDir, "compare.json")), "expected legacy compare.json copy when --json is set");

  const md = fs.readFileSync(path.join(outDir, "compare.md"), "utf8");
  assert.match(md, /## Context/);
  assert.match(md, /config\.seq_total_mb/);

  const summary = JSON.parse(fs.readFileSync(path.join(outDir, "summary.json"), "utf8"));
  assert.equal(summary.status, "pass");
});

test("compare_storage_benchmarks CLI respects STORAGE_PERF_REGRESSION_THRESHOLD_PCT override", () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "aero-storage-compare-"));
  const baselinePath = path.join(tmpDir, "baseline.json");
  const candidatePath = path.join(tmpDir, "candidate.json");
  const thresholdsPath = path.join(tmpDir, "thresholds.json");
  const outDir = path.join(tmpDir, "out");

  // 18% drop would fail the default 15% threshold, but should pass when env sets 20.
  const base = makeStorageBench();
  base.sequential_write = { ...base.sequential_write, mean_mb_per_s: 100, runs: [{ bytes: 1, duration_ms: 1, mb_per_s: 100 }, { bytes: 1, duration_ms: 1, mb_per_s: 100 }] };
  writeJson(baselinePath, base);

  const cand = makeStorageBench();
  cand.sequential_write = { ...cand.sequential_write, mean_mb_per_s: 82, runs: [{ bytes: 1, duration_ms: 1, mb_per_s: 82 }, { bytes: 1, duration_ms: 1, mb_per_s: 82 }] };
  writeJson(candidatePath, cand);
  writeJson(thresholdsPath, makeThresholdPolicy());

  const res = runCompare(
    [
      "--baseline",
      baselinePath,
      "--current",
      candidatePath,
      "--outDir",
      outDir,
      "--thresholds-file",
      thresholdsPath,
      "--profile",
      "pr-smoke",
      "--json",
    ],
    { env: { STORAGE_PERF_REGRESSION_THRESHOLD_PCT: "20" } },
  );

  assert.equal(res.status, 0, `expected exit=0, got ${res.status}\nstdout:\n${res.stdout}\nstderr:\n${res.stderr}`);
  assert.ok(fs.existsSync(path.join(outDir, "compare.md")));
  assert.ok(fs.existsSync(path.join(outDir, "summary.json")));
  assert.ok(fs.existsSync(path.join(outDir, "compare.json")));
});

test("compare_storage_benchmarks CLI exits non-zero on regression and still writes compare.md", () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "aero-storage-compare-"));
  const baselinePath = path.join(tmpDir, "baseline.json");
  const candidatePath = path.join(tmpDir, "candidate.json");
  const thresholdsPath = path.join(tmpDir, "thresholds.json");
  const outDir = path.join(tmpDir, "out");

  const base = makeStorageBench();
  base.sequential_write = { ...base.sequential_write, mean_mb_per_s: 100, runs: [{ bytes: 1, duration_ms: 1, mb_per_s: 100 }, { bytes: 1, duration_ms: 1, mb_per_s: 100 }] };
  writeJson(baselinePath, base);

  const cand = makeStorageBench();
  cand.sequential_write = { ...cand.sequential_write, mean_mb_per_s: 60, runs: [{ bytes: 1, duration_ms: 1, mb_per_s: 60 }, { bytes: 1, duration_ms: 1, mb_per_s: 60 }] };
  writeJson(candidatePath, cand);
  writeJson(thresholdsPath, makeThresholdPolicy());

  const res = runCompare([
    "--baseline",
    baselinePath,
    "--current",
    candidatePath,
    "--outDir",
    outDir,
    "--thresholds-file",
    thresholdsPath,
    "--profile",
    "pr-smoke",
  ]);

  assert.equal(res.status, 1, `expected exit=1, got ${res.status}\nstdout:\n${res.stdout}\nstderr:\n${res.stderr}`);
  assert.ok(fs.existsSync(path.join(outDir, "compare.md")));
});
test("compare_storage_benchmarks CLI exits 2 (unstable) when a required metric is missing", () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "aero-storage-compare-"));
  const baselinePath = path.join(tmpDir, "baseline.json");
  const candidatePath = path.join(tmpDir, "candidate.json");
  const thresholdsPath = path.join(tmpDir, "thresholds.json");
  const outDir = path.join(tmpDir, "out");

  writeJson(baselinePath, makeStorageBench());

  const cand = makeStorageBench();
  delete cand.sequential_read;
  writeJson(candidatePath, cand);

  writeJson(thresholdsPath, makeThresholdPolicy());

  const res = runCompare([
    "--baseline",
    baselinePath,
    "--candidate",
    candidatePath,
    "--out-dir",
    outDir,
    "--thresholds-file",
    thresholdsPath,
    "--profile",
    "pr-smoke",
  ]);

  assert.equal(res.status, 2, `expected exit=2, got ${res.status}\nstdout:\n${res.stdout}\nstderr:\n${res.stderr}`);
  assert.ok(fs.existsSync(path.join(outDir, "compare.md")));
  const summary = JSON.parse(fs.readFileSync(path.join(outDir, "summary.json"), "utf8"));
  assert.equal(summary.status, "unstable");
});

test("compare_storage_benchmarks CLI supports --help", () => {
  const res = spawnSync(
    process.execPath,
    ["--experimental-strip-types", "--import", "./scripts/register-ts-strip-loader.mjs", "scripts/compare_storage_benchmarks.ts", "--help"],
    {
      cwd: path.resolve("."),
      encoding: "utf8",
    },
  );

  assert.equal(res.status, 0, `expected exit=0, got ${res.status}`);
  assert.match(res.stdout, /Usage:/);
  assert.match(res.stdout, /--baseline/);
});
