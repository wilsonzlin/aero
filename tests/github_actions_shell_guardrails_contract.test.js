import assert from "node:assert/strict";
import test from "node:test";
import fs from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { listCompositeActionYmlRelPaths } from "./_helpers/github_actions_contract_helpers.js";

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

test("composite actions: forbid bash shell and heredoc inline scripts", async () => {
  const files = await listCompositeActionYmlRelPaths(repoRoot);

  const violations = [];
  for (const rel of files) {
    const content = await fs.readFile(path.join(repoRoot, rel), "utf8");

    if (/\bshell:\s*bash\b/u.test(content)) {
      violations.push({
        file: rel,
        reason: "composite action uses shell: bash (brittle on Windows runners); prefer default shell + Node scripts",
      });
    }

    if (content.includes("<<'NODE'") || content.includes('<<"NODE"') || content.includes("<<NODE")) {
      violations.push({
        file: rel,
        reason: "composite action uses heredoc inline scripts; prefer action-local .mjs scripts for portability",
      });
    }
  }

  assert.deepEqual(violations, []);
});

