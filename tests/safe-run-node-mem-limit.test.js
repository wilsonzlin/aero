import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const repoRoot = fileURLToPath(new URL("..", import.meta.url));

function makeFakeNodeBin() {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "aero-fake-node-"));
  const nodePath = path.join(dir, "node");
  fs.writeFileSync(
    nodePath,
    `#!/bin/bash
set -euo pipefail
exit 0
`,
    { mode: 0o755 },
  );
  return { dir, nodePath };
}

function makeFakeBin(name) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), `aero-fake-${name}-`));
  const binPath = path.join(dir, name);
  fs.writeFileSync(
    binPath,
    `#!/bin/bash
set -euo pipefail
exit 0
`,
    { mode: 0o755 },
  );
  return { dir, binPath };
}

test("safe-run: bumps RLIMIT_AS for node --test when AERO_MEM_LIMIT is unset (WASM heavy)", { skip: process.platform !== "linux" }, () => {
  const { dir } = makeFakeNodeBin();
  try {
    const env = { ...process.env };
    delete env.AERO_MEM_LIMIT;
    delete env.AERO_NODE_TEST_MEM_LIMIT;
    delete env.CARGO_BUILD_JOBS;
    env.AERO_CARGO_BUILD_JOBS = "1";
    env.PATH = `${dir}:${env.PATH ?? ""}`;

    const res = spawnSync("bash", ["scripts/safe-run.sh", "node", "--test", "fake.test.js"], {
      cwd: repoRoot,
      env,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
    });

    assert.equal(res.status, 0, `expected exit 0, got ${res.status}: ${res.stderr}`);
  assert.match(res.stderr, /Memory: 256G\b/, `expected Memory: 256G in stderr, got:\n${res.stderr}`);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test("safe-run: bumps RLIMIT_AS for npm test when AERO_MEM_LIMIT is unset (WASM heavy)", { skip: process.platform !== "linux" }, () => {
  const { dir } = makeFakeBin("npm");
  try {
    const env = { ...process.env };
    delete env.AERO_MEM_LIMIT;
    delete env.AERO_NODE_TEST_MEM_LIMIT;
    delete env.CARGO_BUILD_JOBS;
    env.AERO_CARGO_BUILD_JOBS = "1";
    env.PATH = `${dir}:${env.PATH ?? ""}`;

    const res = spawnSync("bash", ["scripts/safe-run.sh", "npm", "test"], {
      cwd: repoRoot,
      env,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
    });

    assert.equal(res.status, 0, `expected exit 0, got ${res.status}: ${res.stderr}`);
  assert.match(res.stderr, /Memory: 256G\b/, `expected Memory: 256G in stderr, got:\n${res.stderr}`);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test("safe-run: does not bump RLIMIT_AS for non-test npm invocations", { skip: process.platform !== "linux" }, () => {
  const { dir } = makeFakeBin("npm");
  try {
    const env = { ...process.env };
    delete env.AERO_MEM_LIMIT;
    delete env.AERO_NODE_TEST_MEM_LIMIT;
    delete env.CARGO_BUILD_JOBS;
    env.AERO_CARGO_BUILD_JOBS = "1";
    env.PATH = `${dir}:${env.PATH ?? ""}`;

    const res = spawnSync("bash", ["scripts/safe-run.sh", "npm", "run", "build"], {
      cwd: repoRoot,
      env,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
    });

    assert.equal(res.status, 0, `expected exit 0, got ${res.status}: ${res.stderr}`);
    assert.match(res.stderr, /Memory: 12G\b/, `expected Memory: 12G in stderr, got:\n${res.stderr}`);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test("safe-run: respects an explicit AERO_MEM_LIMIT for npm test", { skip: process.platform !== "linux" }, () => {
  const { dir } = makeFakeBin("npm");
  try {
    const env = { ...process.env };
    env.AERO_MEM_LIMIT = "12G";
    delete env.AERO_NODE_TEST_MEM_LIMIT;
    delete env.CARGO_BUILD_JOBS;
    env.AERO_CARGO_BUILD_JOBS = "1";
    env.PATH = `${dir}:${env.PATH ?? ""}`;

    const res = spawnSync("bash", ["scripts/safe-run.sh", "npm", "test"], {
      cwd: repoRoot,
      env,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
    });

    assert.equal(res.status, 0, `expected exit 0, got ${res.status}: ${res.stderr}`);
    assert.match(res.stderr, /Memory: 12G\b/, `expected Memory: 12G in stderr, got:\n${res.stderr}`);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test(
  "safe-run: AERO_NODE_TEST_MEM_LIMIT overrides the default npm test AS limit",
  { skip: process.platform !== "linux" },
  () => {
    const { dir } = makeFakeBin("npm");
    try {
      const env = { ...process.env };
      delete env.AERO_MEM_LIMIT;
      env.AERO_NODE_TEST_MEM_LIMIT = "24G";
      delete env.CARGO_BUILD_JOBS;
      env.AERO_CARGO_BUILD_JOBS = "1";
      env.PATH = `${dir}:${env.PATH ?? ""}`;

      const res = spawnSync("bash", ["scripts/safe-run.sh", "npm", "test"], {
        cwd: repoRoot,
        env,
        encoding: "utf8",
        stdio: ["ignore", "pipe", "pipe"],
      });

      assert.equal(res.status, 0, `expected exit 0, got ${res.status}: ${res.stderr}`);
      assert.match(res.stderr, /Memory: 24G\b/, `expected Memory: 24G in stderr, got:\n${res.stderr}`);
    } finally {
      fs.rmSync(dir, { recursive: true, force: true });
    }
  },
);

test("safe-run: bumps RLIMIT_AS for wasm-pack test when AERO_MEM_LIMIT is unset", { skip: process.platform !== "linux" }, () => {
  const { dir } = makeFakeBin("wasm-pack");
  try {
    const env = { ...process.env };
    delete env.AERO_MEM_LIMIT;
    delete env.AERO_NODE_TEST_MEM_LIMIT;
    delete env.CARGO_BUILD_JOBS;
    env.AERO_CARGO_BUILD_JOBS = "1";
    env.PATH = `${dir}:${env.PATH ?? ""}`;

    const res = spawnSync("bash", ["scripts/safe-run.sh", "wasm-pack", "test", "--node", "fake"], {
      cwd: repoRoot,
      env,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
    });

    assert.equal(res.status, 0, `expected exit 0, got ${res.status}: ${res.stderr}`);
  assert.match(res.stderr, /Memory: 256G\b/, `expected Memory: 256G in stderr, got:\n${res.stderr}`);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test("safe-run: respects an explicit AERO_MEM_LIMIT for node --test", { skip: process.platform !== "linux" }, () => {
  const { dir } = makeFakeNodeBin();
  try {
    const env = { ...process.env };
    env.AERO_MEM_LIMIT = "12G";
    delete env.AERO_NODE_TEST_MEM_LIMIT;
    delete env.CARGO_BUILD_JOBS;
    env.AERO_CARGO_BUILD_JOBS = "1";
    env.PATH = `${dir}:${env.PATH ?? ""}`;

    const res = spawnSync("bash", ["scripts/safe-run.sh", "node", "--test", "fake.test.js"], {
      cwd: repoRoot,
      env,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
    });

    assert.equal(res.status, 0, `expected exit 0, got ${res.status}: ${res.stderr}`);
    assert.match(res.stderr, /Memory: 12G\b/, `expected Memory: 12G in stderr, got:\n${res.stderr}`);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test("safe-run: AERO_NODE_TEST_MEM_LIMIT overrides the default node --test AS limit", { skip: process.platform !== "linux" }, () => {
  const { dir } = makeFakeNodeBin();
  try {
    const env = { ...process.env };
    delete env.AERO_MEM_LIMIT;
    env.AERO_NODE_TEST_MEM_LIMIT = "24G";
    delete env.CARGO_BUILD_JOBS;
    env.AERO_CARGO_BUILD_JOBS = "1";
    env.PATH = `${dir}:${env.PATH ?? ""}`;

    const res = spawnSync("bash", ["scripts/safe-run.sh", "node", "--test", "fake.test.js"], {
      cwd: repoRoot,
      env,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
    });

    assert.equal(res.status, 0, `expected exit 0, got ${res.status}: ${res.stderr}`);
    assert.match(res.stderr, /Memory: 24G\b/, `expected Memory: 24G in stderr, got:\n${res.stderr}`);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});
