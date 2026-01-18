import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(__dirname, "..");
const scriptPath = path.join(repoRoot, "scripts", "check-node-version.mjs");

function parseVersion(raw) {
  const match = raw.trim().match(/^v?(\d+)\.(\d+)\.(\d+)$/);
  assert.ok(match, `unable to parse version: ${JSON.stringify(raw)}`);
  return {
    major: Number(match[1]),
    minor: Number(match[2]),
    patch: Number(match[3]),
  };
}

function olderVersion(version) {
  if (version.patch > 0) return `${version.major}.${version.minor}.${version.patch - 1}`;
  if (version.minor > 0) return `${version.major}.${version.minor - 1}.0`;
  if (version.major > 0) return `${version.major - 1}.0.0`;
  return "0.0.0";
}

function runCheck(overriddenVersion, { env = {} } = {}) {
  // Tests should be deterministic even if the outer environment sets opt-in knobs.
  // In particular, `AERO_CHECK_NODE_QUIET` suppresses the "newer major" note output.
  const baseEnv = { ...process.env };
  delete baseEnv.AERO_ALLOW_UNSUPPORTED_NODE;
  delete baseEnv.AERO_CHECK_NODE_QUIET;
  delete baseEnv.AERO_ENFORCE_NODE_MAJOR;
  delete baseEnv.AERO_NODE_VERSION_OVERRIDE;

  return spawnSync("node", [scriptPath], {
    cwd: repoRoot,
    env: {
      ...baseEnv,
      AERO_NODE_VERSION_OVERRIDE: overriddenVersion,
      ...env,
    },
    encoding: "utf8",
  });
}

test("check-node-version: passes on the exact .nvmrc version", () => {
  const expected = fs.readFileSync(path.join(repoRoot, ".nvmrc"), "utf8").trim();
  const result = runCheck(expected);
  assert.equal(result.status, 0, result.stderr || result.stdout);
});

test("check-node-version: fails on versions older than the CI baseline", () => {
  const expected = parseVersion(fs.readFileSync(path.join(repoRoot, ".nvmrc"), "utf8"));
  const old = olderVersion(expected);
  const result = runCheck(old);
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /Unsupported Node\.js version/i);
});

test("check-node-version: can be bypassed with AERO_ALLOW_UNSUPPORTED_NODE=1", () => {
  const expected = parseVersion(fs.readFileSync(path.join(repoRoot, ".nvmrc"), "utf8"));
  const old = olderVersion(expected);
  const result = runCheck(old, { env: { AERO_ALLOW_UNSUPPORTED_NODE: "1" } });
  assert.equal(result.status, 0, result.stderr || result.stdout);
  assert.match(result.stderr, /AERO_ALLOW_UNSUPPORTED_NODE/i);
});

test("check-node-version: warns but does not fail on newer major versions", () => {
  const expected = parseVersion(fs.readFileSync(path.join(repoRoot, ".nvmrc"), "utf8"));
  const newerMajor = `${expected.major + 3}.0.0`;
  const result = runCheck(newerMajor);
  assert.equal(result.status, 0, result.stderr || result.stdout);
  assert.match(result.stderr, /differs from CI baseline/i);
});

