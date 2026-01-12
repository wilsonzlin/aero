import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const repoRoot = fileURLToPath(new URL("..", import.meta.url));

function makeFakeCargoBinPrintingEnvVar(varName) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "aero-fake-cargo-"));
  const cargoPath = path.join(dir, "cargo");
  fs.writeFileSync(
    cargoPath,
    `#!/bin/bash
set -euo pipefail
printf "%s" "\${${varName}:-}"
`,
    { mode: 0o755 },
  );
  return { dir, cargoPath };
}

function makeFakeCargoBinPrintingWasmTargetRustflags() {
  return makeFakeCargoBinPrintingEnvVar("CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS");
}

function makeFakeCargoBinPrintingRustflagsAndWasmTargetRustflags() {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "aero-fake-cargo-"));
  const cargoPath = path.join(dir, "cargo");
  fs.writeFileSync(
    cargoPath,
    `#!/bin/bash
set -euo pipefail
printf "%s|%s" "\${RUSTFLAGS:-}" "\${CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS:-}"
`,
    { mode: 0o755 },
  );
  return { dir, cargoPath };
}

function hostTarget() {
  const vv = execFileSync("rustc", ["-vV"], { encoding: "utf8" });
  const m = vv.match(/^host:\s*(.+)\s*$/m);
  assert.ok(m, "rustc -vV must include a host target triple");
  return m[1];
}

function cargoTargetRustflagsVar(target) {
  return `CARGO_TARGET_${target.toUpperCase().replace(/[-.]/g, "_")}_RUSTFLAGS`;
}

test("safe-run: uses --threads=<n> for wasm32 targets (rust-lld -flavor wasm)", { skip: process.platform !== "linux" }, () => {
  const { dir } = makeFakeCargoBinPrintingWasmTargetRustflags();
  try {
    const env = { ...process.env };
    delete env.CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS;
    // Ensure `--target` wins over an existing CARGO_BUILD_TARGET value.
    env.CARGO_BUILD_TARGET = "x86_64-unknown-linux-gnu";
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

test("safe-run: strips -Wl,--threads=<n> from global RUSTFLAGS (two-token form)", { skip: process.platform !== "linux" }, () => {
  const { dir } = makeFakeCargoBinPrintingRustflagsAndWasmTargetRustflags();
  try {
    const env = { ...process.env };
    env.RUSTFLAGS = "-C link-arg=-Wl,--threads=7 -C debuginfo=0";
    delete env.CARGO_BUILD_TARGET;
    delete env.CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS;
    delete env.CARGO_BUILD_JOBS;
    env.AERO_CARGO_BUILD_JOBS = "2";
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

    const [rustflags, wasmTargetRustflags] = stdout.split("|");
    assert.match(rustflags, /-C debuginfo=0\b/);
    assert.doesNotMatch(rustflags, /--threads=\d+\b/);
    assert.doesNotMatch(rustflags, /-Wl,--threads=\d+\b/);

    assert.match(wasmTargetRustflags, /-C link-arg=--threads=2\b/);
    assert.doesNotMatch(wasmTargetRustflags, /-Wl,--threads=2\b/);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test("safe-run: strips -Wl,--threads=<n> from global RUSTFLAGS (single-token form)", { skip: process.platform !== "linux" }, () => {
  const { dir } = makeFakeCargoBinPrintingRustflagsAndWasmTargetRustflags();
  try {
    const env = { ...process.env };
    env.RUSTFLAGS = "-Clink-arg=-Wl,--threads=7 -C debuginfo=0";
    delete env.CARGO_BUILD_TARGET;
    delete env.CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS;
    delete env.CARGO_BUILD_JOBS;
    env.AERO_CARGO_BUILD_JOBS = "2";
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

    const [rustflags, wasmTargetRustflags] = stdout.split("|");
    assert.match(rustflags, /-C debuginfo=0\b/);
    assert.doesNotMatch(rustflags, /--threads=\d+\b/);
    assert.doesNotMatch(rustflags, /-Wl,--threads=\d+\b/);

    assert.match(wasmTargetRustflags, /-C link-arg=--threads=2\b/);
    assert.doesNotMatch(wasmTargetRustflags, /-Wl,--threads=2\b/);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test("safe-run: falls back to CARGO_BUILD_TARGET for wasm32 when no --target flag is provided", { skip: process.platform !== "linux" }, () => {
  const { dir } = makeFakeCargoBinPrintingWasmTargetRustflags();
  try {
    const env = { ...process.env };
    delete env.CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS;
    env.CARGO_BUILD_TARGET = "wasm32-unknown-unknown";
    env.PATH = `${dir}:${env.PATH ?? ""}`;

    const stdout = execFileSync("bash", ["scripts/safe-run.sh", "cargo", "build"], {
      cwd: repoRoot,
      encoding: "utf8",
      env,
      stdio: ["ignore", "pipe", "pipe"],
    });

    assert.match(stdout, /-C link-arg=--threads=\d+\b/);
    assert.doesNotMatch(stdout, /-C link-arg=-Wl,--threads=\d+\b/);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test("safe-run: sets CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS to cap wasm lld threads", { skip: process.platform !== "linux" }, () => {
  const { dir } = makeFakeCargoBinPrintingWasmTargetRustflags();
  try {
    const env = { ...process.env };
    delete env.CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS;
    delete env.CARGO_BUILD_JOBS;
    env.AERO_CARGO_BUILD_JOBS = "2";
    env.PATH = `${dir}:${env.PATH ?? ""}`;

    const stdout = execFileSync("bash", ["scripts/safe-run.sh", "cargo", "build"], {
      cwd: repoRoot,
      encoding: "utf8",
      env,
      stdio: ["ignore", "pipe", "pipe"],
    });

    assert.match(stdout, /-C link-arg=--threads=2\b/);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test(
  "safe-run: rewrites CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS -Wl,--threads=<n> into --threads=<n>",
  { skip: process.platform !== "linux" },
  () => {
    const { dir } = makeFakeCargoBinPrintingWasmTargetRustflags();
    try {
      const env = { ...process.env };
      env.CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS = "-C link-arg=-Wl,--threads=7";
      delete env.CARGO_BUILD_JOBS;
      env.AERO_CARGO_BUILD_JOBS = "2";
      env.PATH = `${dir}:${env.PATH ?? ""}`;

      const stdout = execFileSync("bash", ["scripts/safe-run.sh", "cargo", "build"], {
        cwd: repoRoot,
        encoding: "utf8",
        env,
        stdio: ["ignore", "pipe", "pipe"],
      });

      assert.match(stdout, /-C link-arg=--threads=7\b/);
      assert.doesNotMatch(stdout, /-Wl,--threads=7\b/);
    } finally {
      fs.rmSync(dir, { recursive: true, force: true });
    }
  },
);

test("safe-run: uses -Wl,--threads=<n> for native targets (cc -Wl,... passthrough)", { skip: process.platform !== "linux" }, () => {
  const target = hostTarget();
  const varName = cargoTargetRustflagsVar(target);
  const { dir } = makeFakeCargoBinPrintingEnvVar(varName);
  try {
    const env = { ...process.env };
    delete env.RUSTFLAGS;
    delete env.CARGO_BUILD_TARGET;
    delete env[varName];
    env.PATH = `${dir}:${env.PATH ?? ""}`;

    const stdout = execFileSync("bash", ["scripts/safe-run.sh", "cargo", "build"], {
      cwd: repoRoot,
      encoding: "utf8",
      env,
      stdio: ["ignore", "pipe", "pipe"],
    });

    assert.match(stdout, /-C link-arg=-Wl,--threads=\d+\b/);
    assert.doesNotMatch(stdout, /-C link-arg=--threads=\d+\b/);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});
