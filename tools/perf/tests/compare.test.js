import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const repoRoot = path.resolve(fileURLToPath(new URL("../../..", import.meta.url)));

function writeJson(filePath, value) {
  fs.mkdirSync(path.dirname(filePath), { recursive: true });
  fs.writeFileSync(filePath, `${JSON.stringify(value, null, 2)}\n`, "utf8");
}

test("compare.mjs includes perf_export + trace status lines in compare.md", () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "aero-perf-compare-"));
  try {
    const baselinePath = path.join(tmp, "base", "summary.json");
    const candidatePath = path.join(tmp, "head", "summary.json");
    const outDir = path.join(tmp, "compare");

    writeJson(baselinePath, {
      meta: {
        gitSha: "base",
        aeroPerf: {
          exportAvailable: true,
          exportApiTimedOut: true,
          trace: { requested: false, available: false, captured: false, timedOut: false, durationMs: null },
        },
      },
      benchmarks: [
        { name: "chromium_startup_ms", unit: "ms", stats: { median: 10, cv: 0.1, n: 7 } },
        { name: "microbench_ms", unit: "ms", stats: { median: 20, cv: 0.1, n: 7 } },
      ],
    });

    writeJson(candidatePath, {
      meta: {
        gitSha: "head",
        aeroPerf: {
          exportAvailable: false,
          exportApiTimedOut: true,
          trace: { requested: true, available: true, captured: true, timedOut: false, durationMs: 123 },
        },
      },
      benchmarks: [
        { name: "chromium_startup_ms", unit: "ms", stats: { median: 10, cv: 0.1, n: 7 } },
        { name: "microbench_ms", unit: "ms", stats: { median: 20, cv: 0.1, n: 7 } },
      ],
    });

    const res = spawnSync(
      process.execPath,
      [
        path.join(repoRoot, "tools/perf/compare.mjs"),
        "--baseline",
        baselinePath,
        "--candidate",
        candidatePath,
        "--out-dir",
        outDir,
      ],
      { cwd: repoRoot, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] },
    );

    assert.equal(res.status, 0, `expected compare.mjs success, got ${res.status}\n${res.stderr || res.stdout}`);

    const compareMd = fs.readFileSync(path.join(outDir, "compare.md"), "utf8");
    assert.ok(compareMd.includes("Baseline perf export: available (late)"), "expected baseline perf export status line");
    assert.ok(compareMd.includes("Candidate perf export: timed out"), "expected candidate perf export status line");
    assert.ok(compareMd.includes("Baseline trace:"), "expected baseline trace status line");
    assert.ok(compareMd.includes("Candidate trace:"), "expected candidate trace status line");
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});
