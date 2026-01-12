import assert from "node:assert/strict";
import fs from "node:fs";
import test from "node:test";
import { fileURLToPath } from "node:url";

// Guardrail: documentation and CI invoke these scripts via `./scripts/...`.
// If they lose their executable bit in git, those commands fail with
// `Permission denied`.
const referencedScripts = [
  "../scripts/agent-env-setup.sh",
  "../scripts/agent-env.sh",
  "../scripts/build-bootsector.sh",
  "../scripts/ci/check-iac.sh",
  "../scripts/ci/detect-wasm-crate.sh",
  "../scripts/compare-benchmarks.sh",
  "../scripts/mem-limit.sh",
  "../scripts/prepare-freedos.sh",
  "../scripts/prepare-windows7.sh",
  "../scripts/run_limited.sh",
  "../scripts/safe-run.sh",
  "../scripts/test-all.sh",
  "../scripts/with-timeout.sh",
];

test("scripts referenced as ./scripts/*.sh are executable", { skip: process.platform === "win32" }, () => {
  for (const relPath of referencedScripts) {
    const absPath = fileURLToPath(new URL(relPath, import.meta.url));
    assert.ok(fs.existsSync(absPath), `${relPath} is missing`);

    const { mode } = fs.statSync(absPath);
    // Any executable bit (user/group/other) is good enough.
    assert.ok((mode & 0o111) !== 0, `${relPath} is not executable (expected chmod +x / git mode 100755)`);
  }
});
