import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const sourceScriptPath = fileURLToPath(new URL("../scripts/check-node-version.mjs", import.meta.url));

function parseMajor(version) {
  const major = Number.parseInt(String(version).split(".")[0], 10);
  assert.ok(Number.isFinite(major), `failed to parse node major from: ${version}`);
  return major;
}

function setupTempRepo({ nvmrc }) {
  const repoRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aero-check-node-version-"));
  const scriptDest = path.join(repoRoot, "scripts/check-node-version.mjs");
  fs.mkdirSync(path.dirname(scriptDest), { recursive: true });
  fs.copyFileSync(sourceScriptPath, scriptDest);
  fs.writeFileSync(path.join(repoRoot, ".nvmrc"), `${nvmrc}\n`, "utf8");
  return { repoRoot, scriptDest };
}

function runScript({ repoRoot, scriptDest }, { env = {} } = {}) {
  return spawnSync(process.execPath, [scriptDest], {
    cwd: repoRoot,
    encoding: "utf8",
    env: { ...process.env, ...env },
  });
}

test("check-node-version rejects unsupported Node.js versions by default", () => {
  const currentMajor = parseMajor(process.versions.node);
  const expected = `${currentMajor + 1}.0.0`;
  const temp = setupTempRepo({ nvmrc: expected });
  try {
    const res = runScript(temp);
    assert.equal(res.status, 1, `expected exit=1, got ${res.status}\nstdout:\n${res.stdout}\nstderr:\n${res.stderr}`);
    assert.match(res.stderr, /Unsupported Node\.js version/i);
  } finally {
    fs.rmSync(temp.repoRoot, { recursive: true, force: true });
  }
});

test("check-node-version can be bypassed with AERO_ALLOW_UNSUPPORTED_NODE=1", () => {
  const currentMajor = parseMajor(process.versions.node);
  const expected = `${currentMajor + 1}.0.0`;
  const temp = setupTempRepo({ nvmrc: expected });
  try {
    const res = runScript(temp, { env: { AERO_ALLOW_UNSUPPORTED_NODE: "1" } });
    assert.equal(res.status, 0, `expected exit=0, got ${res.status}\nstdout:\n${res.stdout}\nstderr:\n${res.stderr}`);
    assert.match(res.stderr, /AERO_ALLOW_UNSUPPORTED_NODE/i);
  } finally {
    fs.rmSync(temp.repoRoot, { recursive: true, force: true });
  }
});

