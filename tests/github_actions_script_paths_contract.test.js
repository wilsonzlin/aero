import assert from "node:assert/strict";
import test from "node:test";
import fs from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { listCompositeActionYmlRelPaths } from "./_helpers/github_actions_contract_helpers.js";
import { extractRunScripts } from "./_helpers/yaml_run_scripts.js";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const repoRoot = path.resolve(__dirname, "..");

async function readText(relPath) {
  return await fs.readFile(path.join(repoRoot, relPath), "utf8");
}

function extractNodeMjsInvocations(script) {
  // Extremely small parser: look for `node <arg>` where <arg> contains `.mjs`.
  // Handles simple quotes.
  const invocations = [];
  const re = /\bnode\s+("[^"\n]+"|'[^'\n]+'|[^\s\n]+)/gu;
  for (;;) {
    const m = re.exec(script);
    if (!m) break;
    const rawArg = m[1];
    const arg = rawArg.startsWith("'") || rawArg.startsWith('"') ? rawArg.slice(1, -1) : rawArg;
    if (!arg.includes(".mjs")) continue;
    invocations.push({ raw: `node ${rawArg}`, path: arg });
  }
  return invocations;
}

test("composite actions: node script paths must be cwd-independent", async () => {
  const actionFiles = await listCompositeActionYmlRelPaths(repoRoot);

  const violations = [];
  for (const rel of actionFiles) {
    const content = await readText(rel);

    if (content.includes("node .github/actions/") || content.includes("node ./.github/actions/")) {
      violations.push({
        file: rel,
        reason: "uses repo-relative '.github/actions/...' path (breaks when working-directory is not repo root)",
      });
      continue;
    }

    for (const script of extractRunScripts(content)) {
      for (const inv of extractNodeMjsInvocations(script)) {
        const ok =
          inv.raw.includes("github.action_path") ||
          inv.raw.includes("github.workspace") ||
          inv.raw.includes("GITHUB_WORKSPACE") ||
          inv.raw.includes("GITHUB_ACTION_PATH");

        if (!ok) {
          violations.push({
            file: rel,
            reason: `node .mjs invocation is not anchored to github.action_path/workspace: ${inv.raw}`,
          });
        }
      }
    }
  }

  assert.deepEqual(violations, []);
});

