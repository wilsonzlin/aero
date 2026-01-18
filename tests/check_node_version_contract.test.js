import assert from "node:assert/strict";
import test from "node:test";
import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";
import { readFile } from "node:fs/promises";
import { spawnSync } from "node:child_process";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const repoRoot = path.resolve(__dirname, "..");

function parseVersion(version) {
  const match = version.trim().match(/^v?(\d+)\.(\d+)\.(\d+)$/);
  if (!match) return null;
  return {
    major: Number(match[1]),
    minor: Number(match[2]),
    patch: Number(match[3]),
    raw: version.trim().replace(/^v/, ""),
  };
}

async function readExpectedNodeVersion() {
  const nvmrcPath = path.join(repoRoot, ".nvmrc");
  const raw = await readFile(nvmrcPath, "utf8");
  const parsed = parseVersion(raw);
  assert.ok(parsed, "expected .nvmrc to contain an exact Node version (major.minor.patch)");
  return parsed;
}

function runNodeVersionCheck(env) {
  const scriptPath = path.join(repoRoot, "scripts", "check-node-version.mjs");
  const res = spawnSync(process.execPath, [scriptPath], {
    env: { ...process.env, ...env },
    encoding: "utf8",
  });
  const out = `${res.stdout ?? ""}${res.stderr ?? ""}`;
  return { status: res.status, output: out };
}

test("check-node-version: note output is brief for major mismatch", async () => {
  const expected = await readExpectedNodeVersion();
  const override = `${expected.major + 3}.0.0`;
  const { status, output } = runNodeVersionCheck({ AERO_NODE_VERSION_OVERRIDE: override });

  assert.equal(status, 0);
  assert.ok(output.includes("note: Node.js major version differs from CI baseline"), output);
  assert.ok(output.includes(`detected v${override}`), output);
  assert.ok(output.includes(`CI v${expected.raw}`), output);
  assert.ok(output.length < 400, `expected brief output (<400 chars); got ${output.length}`);
});

test("check-node-version: AERO_CHECK_NODE_QUIET suppresses note output", async () => {
  const expected = await readExpectedNodeVersion();
  const override = `${expected.major + 3}.0.0`;
  const { status, output } = runNodeVersionCheck({
    AERO_NODE_VERSION_OVERRIDE: override,
    AERO_CHECK_NODE_QUIET: "1",
  });

  assert.equal(status, 0);
  assert.equal(output, "");
});

test("check-node-version: AERO_ENFORCE_NODE_MAJOR fails on major mismatch", async () => {
  const expected = await readExpectedNodeVersion();
  const override = `${expected.major + 3}.0.0`;
  const { status, output } = runNodeVersionCheck({
    AERO_NODE_VERSION_OVERRIDE: override,
    AERO_ENFORCE_NODE_MAJOR: "1",
  });

  assert.equal(status, 1);
  assert.ok(output.includes("error: Unsupported Node.js major version."), output);
});

