import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const repoRoot = fileURLToPath(new URL("..", import.meta.url));
const scriptsDir = path.join(repoRoot, "scripts");

function setupTempRepoWithSafeRun() {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "aero-safe-run-"));
  const dstScriptsDir = path.join(root, "scripts");
  fs.mkdirSync(dstScriptsDir, { recursive: true });

  for (const script of ["safe-run.sh", "with-timeout.sh", "run_limited.sh"]) {
    fs.copyFileSync(path.join(scriptsDir, script), path.join(dstScriptsDir, script));
  }

  return root;
}

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

test("safe-run: uses existing .cargo-home when CARGO_HOME is unset", { skip: process.platform === "win32" }, () => {
  const repoRoot = setupTempRepoWithSafeRun();
  try {
    fs.mkdirSync(path.join(repoRoot, ".cargo-home"), { recursive: true });

    const env = { ...process.env };
    delete env.AERO_ISOLATE_CARGO_HOME;
    delete env.CARGO_HOME;

    const res = spawnSync("bash", ["scripts/safe-run.sh", "bash", "-c", 'printf "%s" "$CARGO_HOME"'], {
      cwd: repoRoot,
      encoding: "utf8",
      env,
      stdio: ["ignore", "pipe", "pipe"],
    });

    assert.equal(res.status, 0, `expected safe-run to succeed, got ${res.status}\n${res.stderr}`);
    assert.equal(res.stdout, path.join(repoRoot, ".cargo-home"));
  } finally {
    fs.rmSync(repoRoot, { recursive: true, force: true });
  }
});

test(
  "safe-run: uses existing .cargo-home when CARGO_HOME is set to the default ($HOME/.cargo)",
  { skip: process.platform === "win32" },
  () => {
    const repoRoot = setupTempRepoWithSafeRun();
    try {
      fs.mkdirSync(path.join(repoRoot, ".cargo-home"), { recursive: true });

      const env = { ...process.env };
      delete env.AERO_ISOLATE_CARGO_HOME;
      env.HOME = path.join(repoRoot, "home");
      fs.mkdirSync(env.HOME, { recursive: true });
      env.CARGO_HOME = path.join(env.HOME, ".cargo");

      const res = spawnSync("bash", ["scripts/safe-run.sh", "bash", "-c", 'printf "%s" "$CARGO_HOME"'], {
        cwd: repoRoot,
        encoding: "utf8",
        env,
        stdio: ["ignore", "pipe", "pipe"],
      });

      assert.equal(res.status, 0, `expected safe-run to succeed, got ${res.status}\n${res.stderr}`);
      assert.equal(res.stdout, path.join(repoRoot, ".cargo-home"));
    } finally {
      fs.rmSync(repoRoot, { recursive: true, force: true });
    }
  },
);

test("safe-run: preserves a custom CARGO_HOME even when .cargo-home exists", { skip: process.platform === "win32" }, () => {
  const repoRoot = setupTempRepoWithSafeRun();
  try {
    fs.mkdirSync(path.join(repoRoot, ".cargo-home"), { recursive: true });

    const env = { ...process.env };
    delete env.AERO_ISOLATE_CARGO_HOME;
    env.CARGO_HOME = path.join(repoRoot, "custom-cargo-home");

    const res = spawnSync("bash", ["scripts/safe-run.sh", "bash", "-c", 'printf "%s" "$CARGO_HOME"'], {
      cwd: repoRoot,
      encoding: "utf8",
      env,
      stdio: ["ignore", "pipe", "pipe"],
    });

    assert.equal(res.status, 0, `expected safe-run to succeed, got ${res.status}\n${res.stderr}`);
    assert.equal(res.stdout, env.CARGO_HOME);
  } finally {
    fs.rmSync(repoRoot, { recursive: true, force: true });
  }
});

test(
  "safe-run: creates .cargo-home after detecting Cargo package-cache lock contention",
  { skip: process.platform === "win32" },
  () => {
    const repoRoot = setupTempRepoWithSafeRun();
    const { dir } = makeFakeCargoBinPrintingPackageCacheLock();
    try {
      const env = { ...process.env };
      delete env.AERO_ISOLATE_CARGO_HOME;
      delete env.CARGO_HOME;
      env.PATH = `${dir}:${env.PATH ?? ""}`;

      assert.equal(fs.existsSync(path.join(repoRoot, ".cargo-home")), false);

      const res = spawnSync("bash", ["scripts/safe-run.sh", "cargo", "build"], {
        cwd: repoRoot,
        encoding: "utf8",
        env,
        stdio: ["ignore", "pipe", "pipe"],
      });

      assert.equal(res.status, 0, `expected safe-run to succeed, got ${res.status}\n${res.stderr}`);
      // Creating `./.cargo-home` is best-effort (safe-run may be running in a read-only checkout),
      // so accept either successful creation or an explicit warning.
      if (!fs.existsSync(path.join(repoRoot, ".cargo-home"))) {
        assert.match(res.stderr, /failed to create \.\/\.cargo-home/);
      }
      assert.match(res.stderr, /AERO_ISOLATE_CARGO_HOME=1/);
    } finally {
      fs.rmSync(dir, { recursive: true, force: true });
      fs.rmSync(repoRoot, { recursive: true, force: true });
    }
  },
);
