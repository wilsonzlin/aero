import path from "node:path";
import { ensureFileExists, requireEnv, resolveWorkspaceRoot } from "../_shared/github_io.mjs";
import { actionTimeoutMs, spawnSyncChecked } from "../_shared/exec.mjs";

const outputPath = requireEnv("GITHUB_OUTPUT");
ensureFileExists(outputPath);

const workspace = resolveWorkspaceRoot();
const wasmCrateDir = (process.env.INPUT_WASM_CRATE_DIR ?? "").trim();
if (wasmCrateDir) process.env.AERO_WASM_CRATE_DIR = wasmCrateDir;

const detectScript = path.resolve(workspace, "scripts/ci/detect-wasm-crate.mjs");
const res = spawnSyncChecked(process.execPath, [detectScript, "--github-output", outputPath], {
  cwd: workspace,
  env: process.env,
  timeout: actionTimeoutMs(30_000),
});

// spawnSyncChecked already exits non-zero on failure.
void res;
