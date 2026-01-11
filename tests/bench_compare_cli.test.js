import test from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";

const writeJson = async (filePath, value) => {
  await writeFile(filePath, JSON.stringify(value, null, 2));
};

const makeThresholdPolicy = () => ({
  schemaVersion: 1,
  profiles: {
    "pr-smoke": {
      node: {
        metrics: {
          startup_ms: { better: "lower", maxRegressionPct: 0.15, maxValue: 100, extremeCvThreshold: 0.5 },
          json_parse_ops_s: { better: "higher", maxRegressionPct: 0.15, extremeCvThreshold: 0.5 },
          arith_ops_s: { better: "higher", maxRegressionPct: 0.15, extremeCvThreshold: 0.5 },
        },
      },
    },
  },
});

const runCompare = ({
  baseline,
  current,
  thresholdsFile,
  failOnRegression = true,
  outDir,
}) =>
  spawnSync(
    process.execPath,
    [
      "bench/compare",
      "--baseline",
      baseline,
      "--current",
      current,
      "--thresholds-file",
      thresholdsFile,
      "--profile",
      "pr-smoke",
      "--output-md",
      path.join(outDir, "compare.md"),
      "--output-json",
      path.join(outDir, "summary.json"),
      ...(failOnRegression ? ["--fail-on-regression"] : []),
    ],
    { encoding: "utf8" },
  );

test("bench/compare exits 1 on regression when --fail-on-regression is set", async () => {
  const dir = await mkdtemp(path.join(os.tmpdir(), "aero-bench-compare-"));
  try {
    const baselinePath = path.join(dir, "baseline.json");
    const currentPath = path.join(dir, "current.json");
    const thresholdsPath = path.join(dir, "thresholds.json");
    const outDir = path.join(dir, "out");

    await writeJson(thresholdsPath, makeThresholdPolicy());

    await writeJson(baselinePath, {
      schemaVersion: 1,
      meta: { recordedAt: "2026-01-01T00:00:00Z" },
      scenarios: {
        startup: {
          metrics: {
            startup_ms: { unit: "ms", samples: [120, 120, 120] },
          },
        },
        microbench: {
          metrics: {
            json_parse_ops_s: { unit: "ops/s", samples: [100, 100, 100] },
            arith_ops_s: { unit: "ops/s", samples: [1000, 1000, 1000] },
          },
        },
      },
    });

    // Improved vs baseline, but still above maxValue=100 -> should be a regression.
    await writeJson(currentPath, {
      schemaVersion: 1,
      meta: { recordedAt: "2026-01-02T00:00:00Z" },
      scenarios: {
        startup: {
          metrics: {
            startup_ms: { unit: "ms", samples: [110, 110, 110] },
          },
        },
        microbench: {
          metrics: {
            json_parse_ops_s: { unit: "ops/s", samples: [100, 100, 100] },
            arith_ops_s: { unit: "ops/s", samples: [1000, 1000, 1000] },
          },
        },
      },
    });

    const res = runCompare({
      baseline: baselinePath,
      current: currentPath,
      thresholdsFile: thresholdsPath,
      outDir,
      failOnRegression: true,
    });

    assert.equal(res.status, 1, `expected exit=1, got ${res.status}\nstdout:\n${res.stdout}\nstderr:\n${res.stderr}`);

    const summary = JSON.parse(await readFile(path.join(outDir, "summary.json"), "utf8"));
    assert.equal(summary.status, "regression");

    const md = await readFile(path.join(outDir, "compare.md"), "utf8");
    assert.ok(md.includes("# Benchmark regression report"));
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test("bench/compare exits 0 without --fail-on-regression (report-only mode)", async () => {
  const dir = await mkdtemp(path.join(os.tmpdir(), "aero-bench-compare-"));
  try {
    const baselinePath = path.join(dir, "baseline.json");
    const currentPath = path.join(dir, "current.json");
    const thresholdsPath = path.join(dir, "thresholds.json");
    const outDir = path.join(dir, "out");

    await writeJson(thresholdsPath, makeThresholdPolicy());
    await writeJson(baselinePath, {
      schemaVersion: 1,
      scenarios: {
        startup: { metrics: { startup_ms: { unit: "ms", samples: [100, 100, 100] } } },
      },
    });
    await writeJson(currentPath, {
      schemaVersion: 1,
      scenarios: {
        startup: { metrics: { startup_ms: { unit: "ms", samples: [130, 130, 130] } } },
      },
    });

    const res = runCompare({
      baseline: baselinePath,
      current: currentPath,
      thresholdsFile: thresholdsPath,
      outDir,
      failOnRegression: false,
    });

    assert.equal(res.status, 0, `expected exit=0, got ${res.status}\nstdout:\n${res.stdout}\nstderr:\n${res.stderr}`);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

