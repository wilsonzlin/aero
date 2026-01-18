import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const REPO_ROOT = fileURLToPath(new URL("..", import.meta.url));
const TS_STRIP_LOADER_URL = new URL("../scripts/register-ts-strip-loader.mjs", import.meta.url);
const COMPARE_GATEWAY_SCRIPT_PATH = fileURLToPath(new URL("../scripts/compare_gateway_benchmarks.ts", import.meta.url));

function writeJson(filePath, value) {
  fs.writeFileSync(filePath, `${JSON.stringify(value, null, 2)}\n`, "utf8");
}

function runCompare(args) {
  return spawnSync(
    process.execPath,
    [
      "--experimental-strip-types",
      "--import",
      TS_STRIP_LOADER_URL.href,
      COMPARE_GATEWAY_SCRIPT_PATH,
      ...args,
    ],
    {
      cwd: REPO_ROOT,
      encoding: "utf8",
    },
  );
}

function makeGatewayBench(overrides = {}) {
  return {
    meta: { nodeVersion: "v20.0.0", platform: "linux", arch: "x64", mode: "smoke" },
    tcpProxy: {
      rttMs: { n: 3, min: 10, p50: 10, p90: 20, p99: 30, max: 30, mean: 20, stdev: 0, cv: 0.1 },
      throughput: { mibPerSecond: 100, stats: { n: 3, min: 100, max: 100, mean: 100, stdev: 0, cv: 0.1 } },
    },
    doh: {
      qps: 1000,
      qpsStats: { n: 3, min: 1000, max: 1000, mean: 1000, stdev: 0, cv: 0.1 },
      latencyMs: { n: 3, min: 5, p50: 5, p90: 10, p99: 20, max: 20, mean: 10, stdev: 0, cv: 0.1 },
      cache: { hitRatio: 0.5, hits: 50, misses: 50 },
    },
    ...overrides,
  };
}

function makeThresholdPolicy() {
  return {
    schemaVersion: 1,
    profiles: {
      "pr-smoke": {
        gateway: {
          metrics: {
            tcp_rtt_p50_ms: { better: "lower", maxRegressionPct: 0.15, extremeCvThreshold: 0.5 },
          },
        },
      },
    },
  };
}

test("compare_gateway_benchmarks exits 1 on regression beyond threshold", () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "aero-gateway-compare-"));
  const baselinePath = path.join(tmpDir, "baseline.json");
  const candidatePath = path.join(tmpDir, "candidate.json");
  const thresholdsPath = path.join(tmpDir, "thresholds.json");
  const outDir = path.join(tmpDir, "out");

  writeJson(thresholdsPath, makeThresholdPolicy());
  writeJson(baselinePath, makeGatewayBench());
  writeJson(candidatePath, makeGatewayBench({ tcpProxy: { ...makeGatewayBench().tcpProxy, rttMs: { ...makeGatewayBench().tcpProxy.rttMs, p50: 12 } } }));

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

  assert.equal(res.status, 1, `expected exit=1, got ${res.status}\nstdout:\n${res.stdout}\nstderr:\n${res.stderr}`);
  assert.ok(fs.existsSync(path.join(outDir, "compare.md")));
  assert.ok(fs.existsSync(path.join(outDir, "summary.json")));
});

test("compare_gateway_benchmarks exits 2 on unstable CV", () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "aero-gateway-compare-"));
  const baselinePath = path.join(tmpDir, "baseline.json");
  const candidatePath = path.join(tmpDir, "candidate.json");
  const thresholdsPath = path.join(tmpDir, "thresholds.json");
  const outDir = path.join(tmpDir, "out");

  writeJson(thresholdsPath, makeThresholdPolicy());
  writeJson(baselinePath, makeGatewayBench());
  writeJson(
    candidatePath,
    makeGatewayBench({
      tcpProxy: { ...makeGatewayBench().tcpProxy, rttMs: { ...makeGatewayBench().tcpProxy.rttMs, cv: 0.9 } },
    }),
  );

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
  const summary = JSON.parse(fs.readFileSync(path.join(outDir, "summary.json"), "utf8"));
  assert.equal(summary.status, "unstable");
});

