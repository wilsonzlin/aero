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

