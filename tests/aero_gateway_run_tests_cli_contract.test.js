import assert from "node:assert/strict";
import path from "node:path";
import test from "node:test";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

function repoRootFromHere(importMetaUrl) {
  const filename = fileURLToPath(importMetaUrl);
  return path.resolve(path.dirname(filename), "..");
}

function runGatewayTestsCli(repoRoot, args) {
  const scriptAbs = path.join(repoRoot, "backend", "aero-gateway", "scripts", "run-tests.js");
  const res = spawnSync(process.execPath, [scriptAbs, ...args], {
    cwd: repoRoot,
    env: {
      ...process.env,
      // Avoid spamming contract output; the script still returns non-zero on failures.
      AERO_TEST_STDIO: "ignore",
    },
    encoding: "utf8",
    timeout: 120_000,
    maxBuffer: 2 * 1024 * 1024,
  });
  return res;
}

test("aero-gateway test runner: forwards node --test flags passed after --", () => {
  const repoRoot = repoRootFromHere(import.meta.url);
  const res = runGatewayTestsCli(repoRoot, ["--test-name-pattern=a^"]);
  assert.equal(res.signal, null);
  assert.equal(res.status, 0, res.stderr || res.stdout || "expected exit code 0");
});

test("aero-gateway test runner: supports test root arg + node --test flags", () => {
  const repoRoot = repoRootFromHere(import.meta.url);
  const res = runGatewayTestsCli(repoRoot, ["test/property", "--test-name-pattern=a^"]);
  assert.equal(res.signal, null);
  assert.equal(res.status, 0, res.stderr || res.stdout || "expected exit code 0");
});

