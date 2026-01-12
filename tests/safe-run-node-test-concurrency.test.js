import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
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
printf "%s\\n" "$@"
`,
    { mode: 0o755 },
  );
  return { dir, nodePath };
}

test("safe-run: injects --test-concurrency for node --test when unset", { skip: process.platform !== "linux" }, () => {
  const { dir } = makeFakeNodeBin();
  try {
    const env = { ...process.env };
    // Keep the environment deterministic.
    delete env.NODE_OPTIONS;
    delete env.CARGO_BUILD_JOBS;
    env.AERO_CARGO_BUILD_JOBS = "2";
    env.PATH = `${dir}:${env.PATH ?? ""}`;

    const stdout = execFileSync("bash", ["scripts/safe-run.sh", "node", "--test", "fake.test.js"], {
      cwd: repoRoot,
      encoding: "utf8",
      env,
      stdio: ["ignore", "pipe", "pipe"],
    });

    const args = stdout.trim().split("\n").filter(Boolean);
    assert.deepEqual(args, ["--test", "--test-concurrency=2", "fake.test.js"]);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test("safe-run: does not inject --test-concurrency for normal node invocations", { skip: process.platform !== "linux" }, () => {
  const { dir } = makeFakeNodeBin();
  try {
    const env = { ...process.env };
    delete env.NODE_OPTIONS;
    delete env.CARGO_BUILD_JOBS;
    env.AERO_CARGO_BUILD_JOBS = "2";
    env.PATH = `${dir}:${env.PATH ?? ""}`;

    const stdout = execFileSync("bash", ["scripts/safe-run.sh", "node", "-e", "console.log('hi')"], {
      cwd: repoRoot,
      encoding: "utf8",
      env,
      stdio: ["ignore", "pipe", "pipe"],
    });

    const args = stdout.trim().split("\n").filter(Boolean);
    assert.deepEqual(args, ["-e", "console.log('hi')"]);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

