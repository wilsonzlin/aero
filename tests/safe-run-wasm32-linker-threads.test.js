import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const repoRoot = fileURLToPath(new URL("..", import.meta.url));

function makeFakeCargoBin() {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "aero-fake-cargo-"));
  const cargoPath = path.join(dir, "cargo");
  fs.writeFileSync(
    cargoPath,
    `#!/bin/bash
set -euo pipefail
printf "%s" "\${RUSTFLAGS:-}"
`,
    { mode: 0o755 },
  );
  return { dir, cargoPath };
}

test("safe-run: uses --threads=<n> for wasm32 targets (rust-lld -flavor wasm)", { skip: process.platform === "win32" }, () => {
  const { dir } = makeFakeCargoBin();
  try {
    const env = { ...process.env };
    delete env.RUSTFLAGS;
    delete env.CARGO_BUILD_TARGET;
    env.PATH = `${dir}:${env.PATH ?? ""}`;

    const stdout = execFileSync(
      "bash",
      ["scripts/safe-run.sh", "cargo", "build", "--target", "wasm32-unknown-unknown"],
      {
        cwd: repoRoot,
        encoding: "utf8",
        env,
        stdio: ["ignore", "pipe", "pipe"],
      },
    );

    assert.match(stdout, /-C link-arg=--threads=\d+\b/);
    assert.doesNotMatch(stdout, /-C link-arg=-Wl,--threads=\d+\b/);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test("safe-run: uses -Wl,--threads=<n> for native targets (cc -Wl,... passthrough)", { skip: process.platform === "win32" }, () => {
  const { dir } = makeFakeCargoBin();
  try {
    const env = { ...process.env };
    delete env.RUSTFLAGS;
    delete env.CARGO_BUILD_TARGET;
    env.PATH = `${dir}:${env.PATH ?? ""}`;

    const stdout = execFileSync("bash", ["scripts/safe-run.sh", "cargo", "build"], {
      cwd: repoRoot,
      encoding: "utf8",
      env,
      stdio: ["ignore", "pipe", "pipe"],
    });

    assert.match(stdout, /-C link-arg=-Wl,--threads=\d+\b/);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

