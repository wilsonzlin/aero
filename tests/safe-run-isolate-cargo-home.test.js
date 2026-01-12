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

test(
  "safe-run: AERO_ISOLATE_CARGO_HOME=1 overrides an existing CARGO_HOME and creates .cargo-home",
  { skip: process.platform === "win32" },
  () => {
    const repoRoot = setupTempRepoWithSafeRun();
    try {
      const env = { ...process.env };
      env.AERO_ISOLATE_CARGO_HOME = "1";
      env.CARGO_HOME = path.join(repoRoot, "custom-cargo-home");

      const res = spawnSync("bash", ["scripts/safe-run.sh", "bash", "-c", 'printf "%s" "$CARGO_HOME"'], {
        cwd: repoRoot,
        encoding: "utf8",
        env,
        stdio: ["ignore", "pipe", "pipe"],
      });

      assert.equal(res.status, 0, `expected safe-run to succeed, got ${res.status}\n${res.stderr}`);
      assert.equal(res.stdout, path.join(repoRoot, ".cargo-home"));
      assert.equal(fs.existsSync(path.join(repoRoot, ".cargo-home")), true);
    } finally {
      fs.rmSync(repoRoot, { recursive: true, force: true });
    }
  },
);

test(
  "safe-run: AERO_ISOLATE_CARGO_HOME=<relative> is interpreted as relative to the repo root",
  { skip: process.platform === "win32" },
  () => {
    const repoRoot = setupTempRepoWithSafeRun();
    try {
      const env = { ...process.env };
      env.AERO_ISOLATE_CARGO_HOME = "cargo-state";
      delete env.CARGO_HOME;

      const res = spawnSync("bash", ["scripts/safe-run.sh", "bash", "-c", 'printf "%s" "$CARGO_HOME"'], {
        cwd: repoRoot,
        encoding: "utf8",
        env,
        stdio: ["ignore", "pipe", "pipe"],
      });

      assert.equal(res.status, 0, `expected safe-run to succeed, got ${res.status}\n${res.stderr}`);
      assert.equal(res.stdout, path.join(repoRoot, "cargo-state"));
      assert.equal(fs.existsSync(path.join(repoRoot, "cargo-state")), true);
    } finally {
      fs.rmSync(repoRoot, { recursive: true, force: true });
    }
  },
);

test(
  "safe-run: AERO_ISOLATE_CARGO_HOME expands ~ using HOME when provided a custom path",
  { skip: process.platform === "win32" },
  () => {
    const repoRoot = setupTempRepoWithSafeRun();
    try {
      const env = { ...process.env };
      env.HOME = path.join(repoRoot, "home");
      fs.mkdirSync(env.HOME, { recursive: true });
      env.AERO_ISOLATE_CARGO_HOME = "~/cargo-state";
      delete env.CARGO_HOME;

      const res = spawnSync("bash", ["scripts/safe-run.sh", "bash", "-c", 'printf "%s" "$CARGO_HOME"'], {
        cwd: repoRoot,
        encoding: "utf8",
        env,
        stdio: ["ignore", "pipe", "pipe"],
      });

      assert.equal(res.status, 0, `expected safe-run to succeed, got ${res.status}\n${res.stderr}`);
      assert.equal(res.stdout, path.join(env.HOME, "cargo-state"));
      assert.equal(fs.existsSync(path.join(env.HOME, "cargo-state")), true);
    } finally {
      fs.rmSync(repoRoot, { recursive: true, force: true });
    }
  },
);

