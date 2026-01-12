import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const actualRepoRoot = fileURLToPath(new URL("..", import.meta.url));

function makeFakeCargoBinPrintingPackageCacheLock() {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "aero-fake-cargo-"));
  const cargoPath = path.join(dir, "cargo");
  fs.writeFileSync(
    cargoPath,
    `#!/bin/bash
set -euo pipefail
echo "Blocking waiting for file lock on package cache" >&2
`,
    { mode: 0o755 },
  );
  return { dir, cargoPath };
}

function setupTempSafeRunRepo() {
  const repoRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aero-safe-run-lock-hint-"));
  const scriptsDir = path.join(repoRoot, "scripts");
  fs.mkdirSync(scriptsDir, { recursive: true });

  for (const rel of ["safe-run.sh", "with-timeout.sh", "run_limited.sh"]) {
    const src = path.join(actualRepoRoot, "scripts", rel);
    const dst = path.join(scriptsDir, rel);
    fs.copyFileSync(src, dst);
    fs.chmodSync(dst, 0o755);
  }

  return repoRoot;
}

test("safe-run: hints about AERO_ISOLATE_CARGO_HOME when Cargo hits package cache lock contention", () => {
  const { dir } = makeFakeCargoBinPrintingPackageCacheLock();
  const repoRoot = setupTempSafeRunRepo();
  try {
    const env = { ...process.env };
    delete env.AERO_ISOLATE_CARGO_HOME;
    delete env.CARGO_HOME;
    env.HOME = path.join(repoRoot, "home");
    fs.mkdirSync(env.HOME, { recursive: true });
    env.PATH = `${dir}${path.delimiter}${env.PATH ?? ""}`;

    const res = spawnSync("bash", ["scripts/safe-run.sh", "cargo", "build"], {
      cwd: repoRoot,
      encoding: "utf8",
      env,
      stdio: ["ignore", "pipe", "pipe"],
    });

    assert.equal(res.status, 0, `expected safe-run to succeed, got ${res.status}\n${res.stderr}`);
    assert.match(res.stderr, /Blocking waiting for file lock on package cache/);
    assert.match(res.stderr, /AERO_ISOLATE_CARGO_HOME=1/);
    assert.ok(
      fs.existsSync(path.join(repoRoot, ".cargo-home")),
      "expected safe-run to create ./.cargo-home to reduce Cargo lock contention on future runs",
    );
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
    fs.rmSync(repoRoot, { recursive: true, force: true });
  }
});
