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
