import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const agentEnvPath = fileURLToPath(new URL("../scripts/agent-env.sh", import.meta.url));

function setupTempRepo() {
  const repoRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aero-agent-env-"));
  const scriptsDir = path.join(repoRoot, "scripts");
  fs.mkdirSync(scriptsDir, { recursive: true });
  fs.copyFileSync(agentEnvPath, path.join(scriptsDir, "agent-env.sh"));
  return repoRoot;
}

test("agent-env: AERO_ISOLATE_CARGO_HOME overrides an existing CARGO_HOME", { skip: process.platform === "win32" }, () => {
  const repoRoot = setupTempRepo();
  try {
    const customCargoHome = path.join(repoRoot, "custom-cargo-home");
    const stdout = execFileSync("bash", ["-c", 'source scripts/agent-env.sh >/dev/null; printf "%s" "$CARGO_HOME"'], {
      cwd: repoRoot,
      encoding: "utf8",
      env: {
        ...process.env,
        AERO_ISOLATE_CARGO_HOME: "1",
        CARGO_HOME: customCargoHome,
      },
      stdio: ["ignore", "pipe", "pipe"],
    });

    assert.equal(stdout, path.join(repoRoot, ".cargo-home"));
  } finally {
    fs.rmSync(repoRoot, { recursive: true, force: true });
  }
});

test("agent-env: AERO_ISOLATE_CARGO_HOME accepts a custom path value", { skip: process.platform === "win32" }, () => {
  const repoRoot = setupTempRepo();
  try {
    const stdout = execFileSync("bash", ["-c", 'source scripts/agent-env.sh >/dev/null; printf "%s" "$CARGO_HOME"'], {
      cwd: repoRoot,
      encoding: "utf8",
      env: {
        ...process.env,
        // Relative values should be interpreted relative to the repo root.
        AERO_ISOLATE_CARGO_HOME: "my-cargo-home",
      },
      stdio: ["ignore", "pipe", "pipe"],
    });

    assert.equal(stdout, path.join(repoRoot, "my-cargo-home"));
    assert.ok(fs.existsSync(stdout), `expected custom cargo home directory to exist: ${stdout}`);
  } finally {
    fs.rmSync(repoRoot, { recursive: true, force: true });
  }
});

test("agent-env: AERO_ISOLATE_CARGO_HOME expands ~ using HOME", { skip: process.platform === "win32" }, () => {
  const repoRoot = setupTempRepo();
  const tmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aero-agent-env-home-"));
  try {
    const homeDir = path.join(tmpRoot, "home");
    fs.mkdirSync(homeDir, { recursive: true });

    const stdout = execFileSync("bash", ["-c", 'source scripts/agent-env.sh >/dev/null; printf "%s" "$CARGO_HOME"'], {
      cwd: repoRoot,
      encoding: "utf8",
      env: {
        ...process.env,
        HOME: homeDir,
        AERO_ISOLATE_CARGO_HOME: "~/my-cargo-home",
      },
      stdio: ["ignore", "pipe", "pipe"],
    });

    assert.equal(stdout, path.join(homeDir, "my-cargo-home"));
    assert.ok(fs.existsSync(stdout), `expected expanded cargo home directory to exist: ${stdout}`);
  } finally {
    fs.rmSync(repoRoot, { recursive: true, force: true });
    fs.rmSync(tmpRoot, { recursive: true, force: true });
  }
});

test("agent-env: sets AERO_ALLOW_UNSUPPORTED_NODE when Node major differs from .nvmrc", { skip: process.platform === "win32" }, () => {
  const repoRoot = setupTempRepo();
  try {
    const currentMajor = Number(process.versions.node.split(".")[0]);
    const differentMajor = Number.isFinite(currentMajor) ? currentMajor + 1 : 999;
    fs.writeFileSync(path.join(repoRoot, ".nvmrc"), `${differentMajor}.0.0\n`, "utf8");

    const env = { ...process.env };
    delete env.AERO_ALLOW_UNSUPPORTED_NODE;

    const stdout = execFileSync("bash", ["-c", 'source scripts/agent-env.sh >/dev/null; printf "%s" "${AERO_ALLOW_UNSUPPORTED_NODE:-}"'], {
      cwd: repoRoot,
      encoding: "utf8",
      env,
      stdio: ["ignore", "pipe", "pipe"],
    });

    assert.equal(stdout, "1");
  } finally {
    fs.rmSync(repoRoot, { recursive: true, force: true });
  }
});

test("agent-env: ignores a leading v prefix in .nvmrc when comparing Node majors", { skip: process.platform === "win32" }, () => {
  const repoRoot = setupTempRepo();
  try {
    const currentMajor = Number(process.versions.node.split(".")[0]);
    assert.ok(Number.isFinite(currentMajor) && currentMajor > 0, `unexpected Node major: ${process.versions.node}`);
    fs.writeFileSync(path.join(repoRoot, ".nvmrc"), `v${currentMajor}.0.0\n`, "utf8");

    const env = { ...process.env };
    delete env.AERO_ALLOW_UNSUPPORTED_NODE;

    const stdout = execFileSync("bash", ["-c", 'source scripts/agent-env.sh >/dev/null; printf "%s" "${AERO_ALLOW_UNSUPPORTED_NODE:-}"'], {
      cwd: repoRoot,
      encoding: "utf8",
      env,
      stdio: ["ignore", "pipe", "pipe"],
    });

    assert.equal(stdout, "");
  } finally {
    fs.rmSync(repoRoot, { recursive: true, force: true });
  }
});

test(
  "agent-env: does not override an explicit AERO_ALLOW_UNSUPPORTED_NODE value when .nvmrc major mismatches",
  { skip: process.platform === "win32" },
  () => {
    const repoRoot = setupTempRepo();
    try {
      const currentMajor = Number(process.versions.node.split(".")[0]);
      const differentMajor = Number.isFinite(currentMajor) ? currentMajor + 1 : 999;
      fs.writeFileSync(path.join(repoRoot, ".nvmrc"), `${differentMajor}.0.0\n`, "utf8");

      const stdout = execFileSync(
        "bash",
        ["-c", 'source scripts/agent-env.sh >/dev/null; printf "%s" "${AERO_ALLOW_UNSUPPORTED_NODE:-}"'],
        {
          cwd: repoRoot,
          encoding: "utf8",
          env: {
            ...process.env,
            AERO_ALLOW_UNSUPPORTED_NODE: "0",
          },
          stdio: ["ignore", "pipe", "pipe"],
        },
      );

      assert.equal(stdout, "0");
    } finally {
      fs.rmSync(repoRoot, { recursive: true, force: true });
    }
  },
);

test("agent-env: clears sccache rustc wrapper variables", { skip: process.platform === "win32" }, () => {
  const repoRoot = setupTempRepo();
  try {
    const stdout = execFileSync(
      "bash",
      [
        "-c",
        'source scripts/agent-env.sh >/dev/null; printf "%s|%s|%s|%s" "$RUSTC_WRAPPER" "$RUSTC_WORKSPACE_WRAPPER" "$CARGO_BUILD_RUSTC_WRAPPER" "$CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER"',
      ],
      {
        cwd: repoRoot,
        encoding: "utf8",
        env: {
          ...process.env,
          RUSTC_WRAPPER: "sccache",
          RUSTC_WORKSPACE_WRAPPER: "/usr/bin/sccache",
          CARGO_BUILD_RUSTC_WRAPPER: "sccache",
          CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER: "sccache",
        },
        stdio: ["ignore", "pipe", "pipe"],
      },
    );

    assert.equal(stdout, "|||");
  } finally {
    fs.rmSync(repoRoot, { recursive: true, force: true });
  }
});

test("agent-env: preserves non-sccache rustc wrappers", { skip: process.platform === "win32" }, () => {
  const repoRoot = setupTempRepo();
  try {
    const stdout = execFileSync("bash", ["-c", 'source scripts/agent-env.sh >/dev/null; printf "%s" "$RUSTC_WRAPPER"'], {
      cwd: repoRoot,
      encoding: "utf8",
      env: {
        ...process.env,
        RUSTC_WRAPPER: "ccache",
      },
      stdio: ["ignore", "pipe", "pipe"],
    });

    assert.equal(stdout, "ccache");
  } finally {
    fs.rmSync(repoRoot, { recursive: true, force: true });
  }
});

test(
  "agent-env: AERO_DISABLE_RUSTC_WRAPPER forces wrappers off (even non-sccache)",
  { skip: process.platform === "win32" },
  () => {
    const repoRoot = setupTempRepo();
    try {
      const stdout = execFileSync(
        "bash",
        [
          "-c",
          'source scripts/agent-env.sh >/dev/null; printf "%s|%s|%s|%s" "$RUSTC_WRAPPER" "$RUSTC_WORKSPACE_WRAPPER" "$CARGO_BUILD_RUSTC_WRAPPER" "$CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER"',
        ],
        {
          cwd: repoRoot,
          encoding: "utf8",
          env: {
            ...process.env,
            AERO_DISABLE_RUSTC_WRAPPER: "1",
            RUSTC_WRAPPER: "ccache",
            RUSTC_WORKSPACE_WRAPPER: "/usr/bin/ccache",
            CARGO_BUILD_RUSTC_WRAPPER: "ccache",
            CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER: "ccache",
          },
          stdio: ["ignore", "pipe", "pipe"],
        },
      );

      assert.equal(stdout, "|||");
    } finally {
      fs.rmSync(repoRoot, { recursive: true, force: true });
    }
  },
);

test("agent-env: AERO_CARGO_BUILD_JOBS controls CARGO_BUILD_JOBS", { skip: process.platform === "win32" }, () => {
  const repoRoot = setupTempRepo();
  try {
    const stdout = execFileSync("bash", ["-c", 'source scripts/agent-env.sh >/dev/null; printf "%s" "$CARGO_BUILD_JOBS"'], {
      cwd: repoRoot,
      encoding: "utf8",
      env: {
        ...process.env,
        AERO_CARGO_BUILD_JOBS: "2",
      },
      stdio: ["ignore", "pipe", "pipe"],
    });

    assert.equal(stdout, "2");
  } finally {
    fs.rmSync(repoRoot, { recursive: true, force: true });
  }
});

test("agent-env: defaults rustc/rayon/test threads to CARGO_BUILD_JOBS", { skip: process.platform === "win32" }, () => {
  const repoRoot = setupTempRepo();
  try {
    const env = { ...process.env };
    env.AERO_CARGO_BUILD_JOBS = "2";
    delete env.RUSTC_WORKER_THREADS;
    delete env.RAYON_NUM_THREADS;
    delete env.RUST_TEST_THREADS;

    const stdout = execFileSync(
      "bash",
      ["-c", 'source scripts/agent-env.sh >/dev/null; printf "%s|%s|%s" "$RUSTC_WORKER_THREADS" "$RAYON_NUM_THREADS" "$RUST_TEST_THREADS"'],
      {
        cwd: repoRoot,
        encoding: "utf8",
        env,
        stdio: ["ignore", "pipe", "pipe"],
      },
    );

    assert.equal(stdout, "2|2|2");
  } finally {
    fs.rmSync(repoRoot, { recursive: true, force: true });
  }
});

test("agent-env: invalid AERO_CARGO_BUILD_JOBS falls back to the default", { skip: process.platform === "win32" }, () => {
  const repoRoot = setupTempRepo();
  try {
    const proc = execFileSync("bash", ["-c", 'source scripts/agent-env.sh >/dev/null; printf "%s" "$CARGO_BUILD_JOBS"'], {
      cwd: repoRoot,
      encoding: "utf8",
      env: {
        ...process.env,
        AERO_CARGO_BUILD_JOBS: "nope",
      },
      stdio: ["ignore", "pipe", "pipe"],
    });

    assert.equal(proc, "1");
  } finally {
    fs.rmSync(repoRoot, { recursive: true, force: true });
  }
});

test("agent-env: AERO_TOKIO_WORKER_THREADS defaults to CARGO_BUILD_JOBS", { skip: process.platform === "win32" }, () => {
  const repoRoot = setupTempRepo();
  try {
    const env = { ...process.env };
    env.AERO_CARGO_BUILD_JOBS = "2";
    delete env.AERO_TOKIO_WORKER_THREADS;

    const stdout = execFileSync(
      "bash",
      ["-c", 'source scripts/agent-env.sh >/dev/null; printf "%s" "$AERO_TOKIO_WORKER_THREADS"'],
      {
        cwd: repoRoot,
        encoding: "utf8",
        env,
        stdio: ["ignore", "pipe", "pipe"],
      },
    );

    assert.equal(stdout, "2");
  } finally {
    fs.rmSync(repoRoot, { recursive: true, force: true });
  }
});

test("agent-env: preserves an explicit AERO_TOKIO_WORKER_THREADS", { skip: process.platform === "win32" }, () => {
  const repoRoot = setupTempRepo();
  try {
    const stdout = execFileSync(
      "bash",
      ["-c", 'source scripts/agent-env.sh >/dev/null; printf "%s" "$AERO_TOKIO_WORKER_THREADS"'],
      {
        cwd: repoRoot,
        encoding: "utf8",
        env: {
          ...process.env,
          AERO_CARGO_BUILD_JOBS: "2",
          AERO_TOKIO_WORKER_THREADS: "7",
        },
        stdio: ["ignore", "pipe", "pipe"],
      },
    );

    assert.equal(stdout, "7");
  } finally {
    fs.rmSync(repoRoot, { recursive: true, force: true });
  }
});

test("agent-env: sanitizes invalid AERO_TOKIO_WORKER_THREADS", { skip: process.platform === "win32" }, () => {
  const repoRoot = setupTempRepo();
  try {
    const stdout = execFileSync(
      "bash",
      ["-c", 'source scripts/agent-env.sh >/dev/null; printf "%s" "$AERO_TOKIO_WORKER_THREADS"'],
      {
        cwd: repoRoot,
        encoding: "utf8",
        env: {
          ...process.env,
          AERO_CARGO_BUILD_JOBS: "2",
          AERO_TOKIO_WORKER_THREADS: "nope",
        },
        stdio: ["ignore", "pipe", "pipe"],
      },
    );

    assert.equal(stdout, "2");
  } finally {
    fs.rmSync(repoRoot, { recursive: true, force: true });
  }
});

test("agent-env: NODE_OPTIONS does not include disallowed node flags", { skip: process.platform === "win32" }, () => {
  const repoRoot = setupTempRepo();
  try {
    const env = { ...process.env };
    // Simulate a broken outer environment that injects disallowed node flags via NODE_OPTIONS
    // (Node rejects --test-concurrency in NODE_OPTIONS).
    // Cover both `--test-concurrency=<n>` and `--test-concurrency <n>` spellings.
    env.NODE_OPTIONS = "--trace-warnings --test-concurrency=7 --test-concurrency 3";

    const nodeOptions = execFileSync("bash", ["-c", 'source scripts/agent-env.sh >/dev/null; printf "%s" "$NODE_OPTIONS"'], {
      cwd: repoRoot,
      encoding: "utf8",
      env,
      stdio: ["ignore", "pipe", "pipe"],
    });
    assert.match(nodeOptions, /(^|\s)--trace-warnings(\s|$)/);
    assert.match(nodeOptions, /--max-old-space-size=4096\b/);
    assert.ok(!nodeOptions.includes("--test-concurrency"), `expected NODE_OPTIONS not to include --test-concurrency, got: ${nodeOptions}`);

    const stdout = execFileSync(
      "bash",
      ["-c", 'source scripts/agent-env.sh >/dev/null; node -e "process.stdout.write(\\\"ok\\\")"'],
      {
        cwd: repoRoot,
        encoding: "utf8",
        env,
        stdio: ["ignore", "pipe", "pipe"],
      },
    );
    assert.equal(stdout, "ok");
  } finally {
    fs.rmSync(repoRoot, { recursive: true, force: true });
  }
});

test("agent-env: does not force rustc codegen-units based on CARGO_BUILD_JOBS", { skip: process.platform === "win32" }, () => {
  const repoRoot = setupTempRepo();
  try {
    // `agent-env.sh` caps lld threads via *per-target* rustflags env vars
    // (`CARGO_TARGET_<TRIPLE>_RUSTFLAGS`) instead of mutating `RUSTFLAGS`,
    // because global `RUSTFLAGS` breaks wasm32 builds (`rust-lld -flavor wasm`
    // does not understand `-Wl,...`).
    let hostTarget = null;
    if (process.platform === "linux") {
      try {
        const vv = execFileSync("rustc", ["-vV"], { encoding: "utf8" });
        const m = vv.match(/^host:\s*(.+)\s*$/m);
        hostTarget = m ? m[1] : null;
      } catch {
        hostTarget = null;
      }
    }
    const hostTargetVar =
      hostTarget === null
        ? null
        : `CARGO_TARGET_${hostTarget.toUpperCase().replace(/[-.]/g, "_")}_RUSTFLAGS`;

    const env = { ...process.env };
    delete env.RUSTFLAGS;
    delete env.CARGO_BUILD_JOBS;
    // Keep the test deterministic: if a caller exports CARGO_BUILD_TARGET=wasm32-...,
    // agent-env may rewrite any existing `-Wl,--threads=...` flags in RUSTFLAGS for wasm
    // compatibility. These assertions target the default native path.
    delete env.CARGO_BUILD_TARGET;
    env.AERO_CARGO_BUILD_JOBS = "2";
    delete env.AERO_RUST_CODEGEN_UNITS;
    delete env.AERO_CODEGEN_UNITS;
    if (hostTargetVar) {
      delete env[hostTargetVar];
    }
    delete env.CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS;

    const rustflags = execFileSync("bash", ["-c", 'source scripts/agent-env.sh >/dev/null; printf "%s" "${RUSTFLAGS:-}"'], {
      cwd: repoRoot,
      encoding: "utf8",
      env,
      stdio: ["ignore", "pipe", "pipe"],
    });

    assert.ok(!rustflags.includes("codegen-units="), `expected RUSTFLAGS not to force codegen-units, got: ${rustflags}`);

    // On Linux we cap LLVM lld parallelism to match build jobs, but do it via
    // `CARGO_TARGET_<TRIPLE>_RUSTFLAGS` so wasm builds in the same shell are not broken.
    if (process.platform === "linux") {
      assert.ok(
        !rustflags.includes("--threads=") && !rustflags.includes("-Wl,--threads="),
        `expected agent-env.sh not to mutate global RUSTFLAGS with lld --threads, got: ${rustflags}`,
      );

      if (hostTargetVar) {
        const targetFlags = execFileSync(
          "bash",
          ["-c", 'source scripts/agent-env.sh >/dev/null; printf "%s" "${!AERO_TARGET_RUSTFLAGS_VAR}"'],
          {
            cwd: repoRoot,
            encoding: "utf8",
            env: {
              ...env,
              AERO_TARGET_RUSTFLAGS_VAR: hostTargetVar,
            },
            stdio: ["ignore", "pipe", "pipe"],
          },
        );
        assert.match(targetFlags, /-C link-arg=-Wl,--threads=2\b/);
      }
    }
  } finally {
    fs.rmSync(repoRoot, { recursive: true, force: true });
  }
});

test("agent-env: uses --threads=<n> for wasm32 when CARGO_BUILD_TARGET is set", { skip: process.platform === "win32" }, () => {
  const repoRoot = setupTempRepo();
  try {
    const env = { ...process.env };
    delete env.RUSTFLAGS;
    delete env.CARGO_BUILD_JOBS;
    env.AERO_CARGO_BUILD_JOBS = "2";
    env.CARGO_BUILD_TARGET = "wasm32-unknown-unknown";
    delete env.AERO_RUST_CODEGEN_UNITS;
    delete env.AERO_CODEGEN_UNITS;
    delete env.CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS;

    const stdout = execFileSync("bash", ["-c", 'source scripts/agent-env.sh >/dev/null; printf "%s" "${CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS:-}"'], {
      cwd: repoRoot,
      encoding: "utf8",
      env,
      stdio: ["ignore", "pipe", "pipe"],
    });

    if (process.platform === "linux") {
      assert.match(stdout, /-C link-arg=--threads=2\b/);
      assert.ok(
        !stdout.includes("-Wl,--threads="),
        `expected wasm32 target rustflags to avoid -Wl,--threads, got: ${stdout}`,
      );
    }
  } finally {
    fs.rmSync(repoRoot, { recursive: true, force: true });
  }
});

test("agent-env: rewrites -Wl,--threads=<n> into --threads=<n> for wasm32 targets", { skip: process.platform === "win32" }, () => {
  const repoRoot = setupTempRepo();
  try {
    const env = { ...process.env };
    env.CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS = "-C link-arg=-Wl,--threads=7";
    env.CARGO_BUILD_TARGET = "wasm32-unknown-unknown";

    const stdout = execFileSync(
      "bash",
      ["-c", 'source scripts/agent-env.sh >/dev/null; printf "%s" "$CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS"'],
      {
      cwd: repoRoot,
      encoding: "utf8",
      env,
      stdio: ["ignore", "pipe", "pipe"],
      },
    );

    if (process.platform === "linux") {
      assert.match(stdout, /-C link-arg=--threads=7\b/);
      assert.ok(!stdout.includes("-Wl,--threads="), `expected wasm32 target rustflags to avoid -Wl,--threads, got: ${stdout}`);
    }
  } finally {
    fs.rmSync(repoRoot, { recursive: true, force: true });
  }
});

test("agent-env: strips lld --threads link-args from global RUSTFLAGS (nested wasm safety)", { skip: process.platform !== "linux" }, () => {
  const repoRoot = setupTempRepo();
  try {
    const env = {
      ...process.env,
      AERO_CARGO_BUILD_JOBS: "2",
      // Simulate an outer environment that tried to cap lld threads via global RUSTFLAGS.
      // This should not leak into wasm32 builds.
      RUSTFLAGS: "-C link-arg=-Wl,--threads=99 -Clink-arg=--threads=100 -C opt-level=2",
    };
    // `safe-run.sh` (and some CI/agent sandboxes) can inject per-target rustflags into the outer
    // environment. This test wants to validate that `agent-env.sh` computes the wasm32 threads cap
    // from scratch, so ensure the variable is unset for determinism.
    delete env.CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS;

    const stdout = execFileSync(
      "bash",
      [
        "-c",
        'source scripts/agent-env.sh >/dev/null; printf "%s\\n%s" "${RUSTFLAGS:-}" "${CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS:-}"',
      ],
      {
        cwd: repoRoot,
        encoding: "utf8",
        env,
        stdio: ["ignore", "pipe", "pipe"],
      },
    );

    const [rustflags, wasmFlags] = stdout.split("\n");
    assert.equal(rustflags, "-C opt-level=2");
    assert.match(wasmFlags, /-C link-arg=--threads=2\b/);
    assert.ok(!wasmFlags.includes("-Wl,--threads="), `expected wasm rustflags to avoid -Wl,--threads, got: ${wasmFlags}`);
  } finally {
    fs.rmSync(repoRoot, { recursive: true, force: true });
  }
});

test("agent-env: AERO_RUST_CODEGEN_UNITS overrides codegen-units", { skip: process.platform === "win32" }, () => {
  const repoRoot = setupTempRepo();
  try {
    const env = { ...process.env };
    delete env.RUSTFLAGS;
    delete env.CARGO_BUILD_JOBS;
    delete env.AERO_CARGO_BUILD_JOBS;
    env.AERO_RUST_CODEGEN_UNITS = "2";
    delete env.AERO_CODEGEN_UNITS;

    const stdout = execFileSync("bash", ["-c", 'source scripts/agent-env.sh >/dev/null; printf "%s" "$RUSTFLAGS"'], {
      cwd: repoRoot,
      encoding: "utf8",
      env,
      stdio: ["ignore", "pipe", "pipe"],
    });

    assert.match(stdout, /-C codegen-units=2\b/);
  } finally {
    fs.rmSync(repoRoot, { recursive: true, force: true });
  }
});

test("agent-env: AERO_CODEGEN_UNITS overrides codegen-units (alias)", { skip: process.platform === "win32" }, () => {
  const repoRoot = setupTempRepo();
  try {
    const env = { ...process.env };
    delete env.RUSTFLAGS;
    delete env.CARGO_BUILD_JOBS;
    delete env.AERO_CARGO_BUILD_JOBS;
    delete env.AERO_RUST_CODEGEN_UNITS;
    env.AERO_CODEGEN_UNITS = "2";

    const stdout = execFileSync("bash", ["-c", 'source scripts/agent-env.sh >/dev/null; printf "%s" "$RUSTFLAGS"'], {
      cwd: repoRoot,
      encoding: "utf8",
      env,
      stdio: ["ignore", "pipe", "pipe"],
    });

    assert.match(stdout, /-C codegen-units=2\b/);
  } finally {
    fs.rmSync(repoRoot, { recursive: true, force: true });
  }
});

test("agent-env: preserves an explicit codegen-units setting in RUSTFLAGS", { skip: process.platform === "win32" }, () => {
  const repoRoot = setupTempRepo();
  try {
    const env = { ...process.env, RUSTFLAGS: "-C codegen-units=2" };

    const stdout = execFileSync("bash", ["-c", 'source scripts/agent-env.sh >/dev/null; printf "%s" "$RUSTFLAGS"'], {
      cwd: repoRoot,
      encoding: "utf8",
      env,
      stdio: ["ignore", "pipe", "pipe"],
    });

    assert.ok(stdout.includes("codegen-units=2"), `expected RUSTFLAGS to keep codegen-units=2, got: ${stdout}`);
    assert.ok(!stdout.includes("codegen-units=1"), `expected RUSTFLAGS not to add codegen-units=1, got: ${stdout}`);
  } finally {
    fs.rmSync(repoRoot, { recursive: true, force: true });
  }
});
