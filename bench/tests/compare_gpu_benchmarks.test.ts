import test from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const repoRoot = path.resolve(__dirname, "..", "..");
const compareScriptPath = path.join(repoRoot, "scripts", "compare_gpu_benchmarks.ts");

const writeJson = async (filePath: string, value: unknown) => {
  await writeFile(filePath, JSON.stringify(value, null, 2));
};

const writeThresholds = async (filePath: string) => {
  await writeJson(filePath, {
    schemaVersion: 1,
    profiles: {
      "pr-smoke": {
        gpu: {
          metrics: {
            frameTimeMsP95: { better: "lower", maxRegressionPct: 0.15, extremeCvThreshold: 0.5 },
          },
        },
      },
    },
  });
};

const runCompare = ({
  baseline,
  candidate,
  outDir,
  thresholdsFile,
}: {
  baseline: string;
  candidate: string;
  outDir: string;
  thresholdsFile: string;
}) =>
  spawnSync(
    process.execPath,
    [
      "--experimental-strip-types",
      compareScriptPath,
      "--baseline",
      baseline,
      "--candidate",
      candidate,
      "--out-dir",
      outDir,
      "--thresholds-file",
      thresholdsFile,
      "--profile",
      "pr-smoke",
    ],
    { encoding: "utf8", cwd: repoRoot },
  );

test("compare_gpu_benchmarks exits 1 on regression beyond threshold", async () => {
  const dir = await mkdtemp(path.join(os.tmpdir(), "aero-gpu-compare-"));
  try {
    const baselinePath = path.join(dir, "baseline.json");
    const candidatePath = path.join(dir, "candidate.json");
    const thresholdsPath = path.join(dir, "thresholds.json");
    const outDir = path.join(dir, "out");

    await writeThresholds(thresholdsPath);
    await writeJson(baselinePath, {
      meta: { gitSha: "base", iterations: 3 },
      summary: {
        scenarios: {
          vbe_lfb_blit: {
            name: "VBE LFB full-screen blit",
            status: "ok",
            metrics: { frameTimeMsP95: { median: 100, cv: 0.1, n: 3 } },
          },
        },
      },
    });

    await writeJson(candidatePath, {
      meta: { gitSha: "head", iterations: 3 },
      summary: {
        scenarios: {
          vbe_lfb_blit: {
            name: "VBE LFB full-screen blit",
            status: "ok",
            metrics: { frameTimeMsP95: { median: 120, cv: 0.1, n: 3 } },
          },
        },
      },
    });

    const result = runCompare({ baseline: baselinePath, candidate: candidatePath, outDir, thresholdsFile: thresholdsPath });
    assert.equal(result.status, 1, `expected exit code 1, got ${result.status}\n${result.stderr}`);

    const summary = JSON.parse(await readFile(path.join(outDir, "summary.json"), "utf8"));
    assert.equal(summary.status, "regression");

    const md = await readFile(path.join(outDir, "compare.md"), "utf8");
    assert.ok(md.includes("# GPU perf comparison"), "expected markdown header");
    assert.ok(md.includes("## Context"), "expected context section");
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test("compare_gpu_benchmarks exits 2 on unstable CV", async () => {
  const dir = await mkdtemp(path.join(os.tmpdir(), "aero-gpu-compare-"));
  try {
    const baselinePath = path.join(dir, "baseline.json");
    const candidatePath = path.join(dir, "candidate.json");
    const thresholdsPath = path.join(dir, "thresholds.json");
    const outDir = path.join(dir, "out");

    await writeThresholds(thresholdsPath);
    await writeJson(baselinePath, {
      meta: { gitSha: "base", iterations: 3 },
      summary: {
        scenarios: {
          vbe_lfb_blit: {
            name: "VBE LFB full-screen blit",
            status: "ok",
            metrics: { frameTimeMsP95: { median: 100, cv: 0.1, n: 3 } },
          },
        },
      },
    });

    await writeJson(candidatePath, {
      meta: { gitSha: "head", iterations: 3 },
      summary: {
        scenarios: {
          vbe_lfb_blit: {
            name: "VBE LFB full-screen blit",
            status: "ok",
            metrics: { frameTimeMsP95: { median: 100, cv: 0.9, n: 3 } },
          },
        },
      },
    });

    const result = runCompare({ baseline: baselinePath, candidate: candidatePath, outDir, thresholdsFile: thresholdsPath });
    assert.equal(result.status, 2, `expected exit code 2, got ${result.status}\n${result.stderr}`);

    const summary = JSON.parse(await readFile(path.join(outDir, "summary.json"), "utf8"));
    assert.equal(summary.status, "unstable");
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test("compare_gpu_benchmarks exits 2 when a required candidate metric is missing", async () => {
  const dir = await mkdtemp(path.join(os.tmpdir(), "aero-gpu-compare-"));
  try {
    const baselinePath = path.join(dir, "baseline.json");
    const candidatePath = path.join(dir, "candidate.json");
    const thresholdsPath = path.join(dir, "thresholds.json");
    const outDir = path.join(dir, "out");

    await writeThresholds(thresholdsPath);
    await writeJson(baselinePath, {
      meta: { gitSha: "base", iterations: 3 },
      summary: {
        scenarios: {
          vbe_lfb_blit: {
            name: "VBE LFB full-screen blit",
            status: "ok",
            metrics: { frameTimeMsP95: { median: 100, cv: 0.1, n: 3 } },
          },
        },
      },
    });

    // Candidate intentionally missing frameTimeMsP95.
    await writeJson(candidatePath, {
      meta: { gitSha: "head", iterations: 3 },
      summary: {
        scenarios: {
          vbe_lfb_blit: {
            name: "VBE LFB full-screen blit",
            status: "ok",
            metrics: {},
          },
        },
      },
    });

    const result = runCompare({ baseline: baselinePath, candidate: candidatePath, outDir, thresholdsFile: thresholdsPath });
    assert.equal(result.status, 2, `expected exit code 2, got ${result.status}\n${result.stderr}`);

    const summary = JSON.parse(await readFile(path.join(outDir, "summary.json"), "utf8"));
    assert.equal(summary.status, "unstable");
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test("compare_gpu_benchmarks skips comparisons for metrics missing in both baseline and candidate", async () => {
  const dir = await mkdtemp(path.join(os.tmpdir(), "aero-gpu-compare-"));
  try {
    const baselinePath = path.join(dir, "baseline.json");
    const candidatePath = path.join(dir, "candidate.json");
    const thresholdsPath = path.join(dir, "thresholds.json");
    const outDir = path.join(dir, "out");

    await writeJson(thresholdsPath, {
      schemaVersion: 1,
      profiles: {
        "pr-smoke": {
          gpu: {
            metrics: {
              frameTimeMsP95: { better: "lower", maxRegressionPct: 0.15, extremeCvThreshold: 0.5 },
              presentLatencyMsP95: { better: "lower", maxRegressionPct: 0.15, extremeCvThreshold: 0.5 },
            },
          },
        },
      },
    });

    // Both baseline + candidate intentionally omit presentLatencyMsP95 (e.g. a 2D scenario).
    await writeJson(baselinePath, {
      meta: { gitSha: "base", iterations: 3 },
      summary: {
        scenarios: {
          vga_text_scroll: {
            name: "VGA text scroll stress",
            status: "ok",
            metrics: { frameTimeMsP95: { median: 10, cv: 0.1, n: 3 } },
          },
        },
      },
    });

    await writeJson(candidatePath, {
      meta: { gitSha: "head", iterations: 3 },
      summary: {
        scenarios: {
          vga_text_scroll: {
            name: "VGA text scroll stress",
            status: "ok",
            metrics: { frameTimeMsP95: { median: 10, cv: 0.1, n: 3 } },
          },
        },
      },
    });

    const result = runCompare({ baseline: baselinePath, candidate: candidatePath, outDir, thresholdsFile: thresholdsPath });
    assert.equal(result.status, 0, `expected exit code 0, got ${result.status}\n${result.stderr}`);

    const summary = JSON.parse(await readFile(path.join(outDir, "summary.json"), "utf8"));
    assert.equal(summary.status, "pass");
    assert.ok(Array.isArray(summary.comparisons));
    assert.equal(summary.comparisons.some((c: any) => c.metric === "presentLatencyMsP95"), false);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test("compare_gpu_benchmarks supports aero-gpu-bench schemaVersion=1 baseline", async () => {
  const dir = await mkdtemp(path.join(os.tmpdir(), "aero-gpu-compare-"));
  try {
    const baselinePath = path.join(dir, "baseline.json");
    const candidatePath = path.join(dir, "candidate.json");
    const thresholdsPath = path.join(dir, "thresholds.json");
    const outDir = path.join(dir, "out");

    await writeThresholds(thresholdsPath);
    await writeJson(baselinePath, {
      schemaVersion: 1,
      tool: "aero-gpu-bench",
      startedAt: "2025-01-01T00:00:00Z",
      finishedAt: "2025-01-01T00:00:01Z",
      environment: { userAgent: "UA", webgpu: false, webgl2: true },
      scenarios: {
        vbe_lfb_blit: {
          id: "vbe_lfb_blit",
          name: "VBE LFB full-screen blit",
          status: "ok",
          durationMs: 123,
          params: {},
          telemetry: {},
          derived: { frameTimeMsP95: 100 },
        },
      },
    });

    await writeJson(candidatePath, {
      meta: { gitSha: "head", iterations: 3 },
      summary: {
        scenarios: {
          vbe_lfb_blit: {
            name: "VBE LFB full-screen blit",
            status: "ok",
            metrics: { frameTimeMsP95: { median: 120, cv: 0.1, n: 3 } },
          },
        },
      },
    });

    const result = runCompare({ baseline: baselinePath, candidate: candidatePath, outDir, thresholdsFile: thresholdsPath });
    assert.equal(result.status, 1, `expected exit code 1, got ${result.status}\n${result.stderr}`);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});
