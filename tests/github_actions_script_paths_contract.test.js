import assert from "node:assert/strict";
import test from "node:test";
import fs from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const repoRoot = path.resolve(__dirname, "..");

async function readText(relPath) {
  return await fs.readFile(path.join(repoRoot, relPath), "utf8");
}

test("composite actions: node script paths must be cwd-independent", async () => {
  const actions = [
    ".github/actions/setup-node-workspace/action.yml",
    ".github/actions/setup-rust/action.yml",
    ".github/actions/setup-playwright/action.yml",
    ".github/actions/resolve-wasm-crate/action.yml",
  ];

  const violations = [];
  for (const rel of actions) {
    const content = await readText(rel);

    if (content.includes("node .github/actions/")) {
      violations.push({ file: rel, reason: "uses repo-relative '.github/actions/...' path (breaks when working-directory is not repo root)" });
    }

    if (/run:\s*node\s+"\$\{\{\s*github\.action_path\s*\}\}\/[^\n"]+"/u.test(content) === false) {
      // Not every node call must use action_path (some may call node scripts/...), but each action we
      // refactored should now have at least one action_path usage; otherwise we likely regressed.
      violations.push({ file: rel, reason: "expected at least one node invocation using github.action_path" });
    }
  }

  assert.deepEqual(violations, []);
});

