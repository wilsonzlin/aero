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
    ["--experimental-strip-types", "scripts/compare_storage_benchmarks.ts", ...args],
    {
      cwd: path.resolve("."),
      env: { ...process.env, ...(opts.env ?? {}) },
      encoding: "utf8",
    },
  );
  return result;
}

function makeStorageBench(overrides = {}) {
  return {
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
    sequential_write: { mean_mb_per_s: 100 },
    sequential_read: { mean_mb_per_s: 150 },
    random_read_4k: { mean_p50_ms: 5, mean_p95_ms: 10 },
    ...overrides,
  };
}

test("compare_storage_benchmarks CLI supports --candidate + --out-dir and writes artifacts", () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "aero-storage-compare-"));
  const baselinePath = path.join(tmpDir, "baseline.json");
  const candidatePath = path.join(tmpDir, "candidate.json");
  const outDir = path.join(tmpDir, "out");

  writeJson(baselinePath, makeStorageBench());
  writeJson(candidatePath, makeStorageBench({ sequential_write: { mean_mb_per_s: 101 } }));

  const res = runCompare([
    "--baseline",
    baselinePath,
    "--candidate",
    candidatePath,
    "--out-dir",
    outDir,
    "--thresholdPct",
    "15",
    "--json",
  ]);

  assert.equal(res.status, 0, `expected exit=0, got ${res.status}\nstdout:\n${res.stdout}\nstderr:\n${res.stderr}`);
  assert.ok(fs.existsSync(path.join(outDir, "compare.md")));
  assert.ok(fs.existsSync(path.join(outDir, "compare.json")));
});

test("compare_storage_benchmarks CLI respects STORAGE_PERF_REGRESSION_THRESHOLD_PCT override", () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "aero-storage-compare-"));
  const baselinePath = path.join(tmpDir, "baseline.json");
  const candidatePath = path.join(tmpDir, "candidate.json");
  const outDir = path.join(tmpDir, "out");

  // 18% drop would fail the default 15% threshold, but should pass when env sets 20.
  writeJson(baselinePath, makeStorageBench({ sequential_write: { mean_mb_per_s: 100 } }));
  writeJson(candidatePath, makeStorageBench({ sequential_write: { mean_mb_per_s: 82 } }));

  const res = runCompare(
    ["--baseline", baselinePath, "--current", candidatePath, "--outDir", outDir, "--json"],
    { env: { STORAGE_PERF_REGRESSION_THRESHOLD_PCT: "20" } },
  );

  assert.equal(res.status, 0, `expected exit=0, got ${res.status}\nstdout:\n${res.stdout}\nstderr:\n${res.stderr}`);
  assert.ok(fs.existsSync(path.join(outDir, "compare.md")));
  assert.ok(fs.existsSync(path.join(outDir, "compare.json")));
});

test("compare_storage_benchmarks CLI exits non-zero on regression and still writes compare.md", () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "aero-storage-compare-"));
  const baselinePath = path.join(tmpDir, "baseline.json");
  const candidatePath = path.join(tmpDir, "candidate.json");
  const outDir = path.join(tmpDir, "out");

  writeJson(baselinePath, makeStorageBench({ sequential_write: { mean_mb_per_s: 100 } }));
  writeJson(candidatePath, makeStorageBench({ sequential_write: { mean_mb_per_s: 60 } }));

  const res = runCompare([
    "--baseline",
    baselinePath,
    "--current",
    candidatePath,
    "--outDir",
    outDir,
    "--thresholdPct",
    "15",
  ]);

  assert.equal(res.status, 1, `expected exit=1, got ${res.status}\nstdout:\n${res.stdout}\nstderr:\n${res.stderr}`);
  assert.ok(fs.existsSync(path.join(outDir, "compare.md")));
});

