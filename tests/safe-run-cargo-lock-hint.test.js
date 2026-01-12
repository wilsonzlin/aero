import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const repoRoot = fileURLToPath(new URL("..", import.meta.url));

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

test("safe-run: hints about AERO_ISOLATE_CARGO_HOME when Cargo hits package cache lock contention", () => {
  const { dir } = makeFakeCargoBinPrintingPackageCacheLock();
  try {
    const env = { ...process.env };
    delete env.AERO_ISOLATE_CARGO_HOME;
    env.PATH = `${dir}:${env.PATH ?? ""}`;

    const res = spawnSync("bash", ["scripts/safe-run.sh", "cargo", "build"], {
      cwd: repoRoot,
      encoding: "utf8",
      env,
      stdio: ["ignore", "pipe", "pipe"],
    });

    assert.equal(res.status, 0, `expected safe-run to succeed, got ${res.status}\n${res.stderr}`);
    assert.match(res.stderr, /Blocking waiting for file lock on package cache/);
    assert.match(res.stderr, /AERO_ISOLATE_CARGO_HOME=1/);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

