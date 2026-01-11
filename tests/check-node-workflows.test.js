import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const sourceScriptPath = fileURLToPath(new URL("../scripts/ci/check-node-workflows.mjs", import.meta.url));
const pinnedNodeRaw = fs.readFileSync(fileURLToPath(new URL("../.nvmrc", import.meta.url)), "utf8").trim();
const pinnedNode = pinnedNodeRaw.replace(/^v/, "");
const pinnedMajor = Number(pinnedNode.split(".")[0]);

function writeJson(filePath, value) {
  fs.mkdirSync(path.dirname(filePath), { recursive: true });
  fs.writeFileSync(filePath, `${JSON.stringify(value, null, 2)}\n`);
}

function setupTempRepo() {
  const repoRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aero-node-workflows-"));

  const scriptDest = path.join(repoRoot, "scripts/ci/check-node-workflows.mjs");
  fs.mkdirSync(path.dirname(scriptDest), { recursive: true });
  fs.copyFileSync(sourceScriptPath, scriptDest);

  fs.writeFileSync(path.join(repoRoot, ".nvmrc"), `${pinnedNode}\n`);
  writeJson(path.join(repoRoot, "package.json"), {
    name: "temp",
    private: true,
    type: "module",
    workspaces: [],
    engines: { node: `>=${pinnedNode} <${pinnedMajor + 1}` },
  });

  return { repoRoot, scriptPath: scriptDest };
}

function runCheck({ repoRoot, scriptPath }) {
  return spawnSync(process.execPath, [scriptPath], {
    cwd: repoRoot,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  });
}

test("check-node-workflows: rejects multi-line setup-node steps using node-version", () => {
  const temp = setupTempRepo();
  try {
    const workflowsDir = path.join(temp.repoRoot, ".github/workflows");
    fs.mkdirSync(workflowsDir, { recursive: true });
    fs.writeFileSync(
      path.join(workflowsDir, "bad.yml"),
      `name: bad\non: [push]\n\njobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n      - name: Setup Node\n        uses: actions/setup-node@v4\n        with:\n          node-version: 20.x\n`,
    );

    const res = runCheck(temp);
    assert.notEqual(res.status, 0);
    assert.match(res.stderr, /actions\/setup-node@v4 uses 'node-version:'/);
    assert.match(res.stderr, /missing 'node-version-file: \.nvmrc'/);
  } finally {
    fs.rmSync(temp.repoRoot, { recursive: true, force: true });
  }
});

test("check-node-workflows: accepts multi-line setup-node steps using node-version-file", () => {
  const temp = setupTempRepo();
  try {
    const workflowsDir = path.join(temp.repoRoot, ".github/workflows");
    fs.mkdirSync(workflowsDir, { recursive: true });
    fs.writeFileSync(
      path.join(workflowsDir, "ok.yml"),
      `name: ok\non: [push]\n\njobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n      - name: Setup Node\n        uses: actions/setup-node@v4\n        with:\n          node-version-file: .nvmrc\n`,
    );

    const res = runCheck(temp);
    assert.equal(res.status, 0, `expected exit 0, got ${res.status}\n\nstderr:\n${res.stderr}`);
  } finally {
    fs.rmSync(temp.repoRoot, { recursive: true, force: true });
  }
});

test("check-node-workflows: rejects Dockerfile node base images without an exact semver tag", () => {
  const temp = setupTempRepo();
  try {
    const workflowsDir = path.join(temp.repoRoot, ".github/workflows");
    fs.mkdirSync(workflowsDir, { recursive: true });
    fs.writeFileSync(path.join(workflowsDir, "ok.yml"), "name: ok\non: [push]\njobs: {}\n");

    fs.mkdirSync(path.join(temp.repoRoot, "server"), { recursive: true });
    fs.writeFileSync(path.join(temp.repoRoot, "server/Dockerfile"), `FROM node:${pinnedMajor}-alpine\n`);

    const res = runCheck(temp);
    assert.notEqual(res.status, 0);
    assert.match(res.stderr, /Dockerfile Node base image must use an exact semver tag/);
  } finally {
    fs.rmSync(temp.repoRoot, { recursive: true, force: true });
  }
});

test("check-node-workflows: rejects Dockerfiles with a Node tag out of sync with .nvmrc", () => {
  const temp = setupTempRepo();
  try {
    const workflowsDir = path.join(temp.repoRoot, ".github/workflows");
    fs.mkdirSync(workflowsDir, { recursive: true });
    fs.writeFileSync(path.join(workflowsDir, "ok.yml"), "name: ok\non: [push]\njobs: {}\n");

    fs.mkdirSync(path.join(temp.repoRoot, "server"), { recursive: true });
    fs.writeFileSync(path.join(temp.repoRoot, "server/Dockerfile"), "FROM node:0.0.0-slim\n");

    const res = runCheck(temp);
    assert.notEqual(res.status, 0);
    assert.match(res.stderr, /Dockerfile Node version is out of sync with \.nvmrc/);
    assert.ok(res.stderr.includes(`- expected: ${pinnedNode}`), res.stderr);
  } finally {
    fs.rmSync(temp.repoRoot, { recursive: true, force: true });
  }
});

test("check-node-workflows: accepts Dockerfile node base images pinned to the .nvmrc version", () => {
  const temp = setupTempRepo();
  try {
    const workflowsDir = path.join(temp.repoRoot, ".github/workflows");
    fs.mkdirSync(workflowsDir, { recursive: true });
    fs.writeFileSync(path.join(workflowsDir, "ok.yml"), "name: ok\non: [push]\njobs: {}\n");

    fs.mkdirSync(path.join(temp.repoRoot, "server"), { recursive: true });
    fs.writeFileSync(path.join(temp.repoRoot, "server/Dockerfile"), `FROM node:${pinnedNode}-alpine\n`);

    const res = runCheck(temp);
    assert.equal(res.status, 0, `expected exit 0, got ${res.status}\n\nstderr:\n${res.stderr}`);
  } finally {
    fs.rmSync(temp.repoRoot, { recursive: true, force: true });
  }
});

test("check-node-workflows: accepts Dockerfiles that use an ARG NODE_VERSION default matching .nvmrc", () => {
  const temp = setupTempRepo();
  try {
    const workflowsDir = path.join(temp.repoRoot, ".github/workflows");
    fs.mkdirSync(workflowsDir, { recursive: true });
    fs.writeFileSync(path.join(workflowsDir, "ok.yml"), "name: ok\non: [push]\njobs: {}\n");

    fs.mkdirSync(path.join(temp.repoRoot, "server"), { recursive: true });
    fs.writeFileSync(
      path.join(temp.repoRoot, "server/Dockerfile"),
      `ARG NODE_VERSION=${pinnedNode}\nFROM node:\${NODE_VERSION}-slim\n`,
    );

    const res = runCheck(temp);
    assert.equal(res.status, 0, `expected exit 0, got ${res.status}\n\nstderr:\n${res.stderr}`);
  } finally {
    fs.rmSync(temp.repoRoot, { recursive: true, force: true });
  }
});
