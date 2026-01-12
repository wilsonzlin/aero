import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

// Guardrail: documentation and CI invoke these scripts via `./scripts/...`.
// If they lose their executable bit in git, those commands fail with
// `Permission denied`.
const hardcodedScripts = [
  "scripts/agent-env-setup.sh",
  "scripts/agent-env.sh",
  "scripts/build-bootsector.sh",
  "scripts/ci/check-iac.sh",
  "scripts/ci/detect-wasm-crate.sh",
  "scripts/compare-benchmarks.sh",
  "scripts/mem-limit.sh",
  "scripts/prepare-freedos.sh",
  "scripts/prepare-windows7.sh",
  "scripts/run_limited.sh",
  "scripts/safe-run.sh",
  "scripts/test-all.sh",
  "scripts/with-timeout.sh",
];

const repoRoot = fileURLToPath(new URL("..", import.meta.url));

function scriptsReferencedByDocs() {
  // Only scan tracked Markdown files to avoid walking huge untracked dirs like
  // node_modules/ or target/ (which may exist in CI/local checkouts).
  const markdownFiles = execFileSync("git", ["ls-files"], {
    cwd: repoRoot,
    encoding: "utf8",
  })
    .split("\n")
    .map((line) => line.trim())
    .filter((line) => line.length > 0 && line.endsWith(".md"));

  // Match docs that invoke a shell script directly, e.g.:
  //   ./scripts/safe-run.sh cargo test --locked
  // Keep the character class conservative; `.sh` is an unambiguous terminator.
  const re = /\.\/scripts\/[0-9A-Za-z_./-]+\.sh/g;

  const scripts = new Set();
  for (const relDocPath of markdownFiles) {
    const absDocPath = path.join(repoRoot, relDocPath);
    const content = fs.readFileSync(absDocPath, "utf8");
    const matches = content.match(re) ?? [];
    for (const match of matches) {
      scripts.add(match.replace(/^\.\//, ""));
    }
  }
  return scripts;
}

function scriptsToCheck() {
  const scripts = new Set(hardcodedScripts);
  for (const docScript of scriptsReferencedByDocs()) scripts.add(docScript);
  return [...scripts].sort();
}

test("scripts referenced as ./scripts/*.sh are executable", { skip: process.platform === "win32" }, () => {
  for (const relPath of scriptsToCheck()) {
    const absPath = path.join(repoRoot, relPath);
    assert.ok(fs.existsSync(absPath), `${relPath} is missing`);

    const { mode } = fs.statSync(absPath);
    // Any executable bit (user/group/other) is good enough.
    assert.ok(
      (mode & 0o111) !== 0,
      `${relPath} is not executable (expected chmod +x / git mode 100755)`,
    );
  }
});
