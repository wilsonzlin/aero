import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const repoRoot = fileURLToPath(new URL("..", import.meta.url));

function extractAeroEnvVarsFromFile(relPath) {
  const absPath = path.join(repoRoot, relPath);
  const contents = fs.readFileSync(absPath, "utf8");
  const matches = contents.match(/\bAERO_[A-Z0-9_]+\b/g) ?? [];
  return new Set(matches);
}

test("docs/env-vars.md documents env vars used by agent helper scripts", () => {
  const docsPath = path.join(repoRoot, "docs", "env-vars.md");
  const docs = fs.readFileSync(docsPath, "utf8");

  // `scripts/agent-env.sh` and `scripts/safe-run.sh` are used heavily in agent sandboxes. We want
  // the canonical env var reference (`docs/env-vars.md`) to stay in sync with the knobs those
  // scripts actually parse, so future additions don't silently go undocumented.
  const required = new Set([
    ...extractAeroEnvVarsFromFile("scripts/agent-env.sh"),
    ...extractAeroEnvVarsFromFile("scripts/safe-run.sh"),
  ]);

  // A few non-AERO knobs are part of the "agent defaults" contract and are documented in the same
  // table. Keep them covered too.
  for (const name of [
    "RUSTC_WORKER_THREADS",
    "RAYON_NUM_THREADS",
    "RUST_TEST_THREADS",
    "NEXTEST_TEST_THREADS",
  ]) {
    required.add(name);
  }

  // Ensure we keep a minimal anchor for the L2 proxy config section.
  for (const name of ["AERO_L2_PROXY_LISTEN_ADDR", "AERO_L2_AUTH_MODE", "AERO_L2_OPEN"]) {
    required.add(name);
  }

  const missing = [];
  for (const name of [...required].sort()) {
    if (!new RegExp(`\\b${name}\\b`).test(docs)) {
      missing.push(name);
    }
  }

  assert.deepEqual(
    missing,
    [],
    `docs/env-vars.md is missing entries for env vars used by agent helper scripts:\n${missing.join("\n")}`,
  );
});

