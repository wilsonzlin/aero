import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { listFilesRecursive } from "./_helpers/fs_walk.js";
import { commandHasTsStripLoaderImport, commandRunsTsWithStripTypes, normalizeShellLineContinuations } from "./_helpers/ts_strip_loader_contract_helpers.js";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const repoRoot = path.resolve(__dirname, "..");

async function readText(relPath) {
  return await fs.readFile(path.join(repoRoot, relPath), "utf8");
}

function leadingSpaces(line) {
  let n = 0;
  while (n < line.length && line.charCodeAt(n) === 32) n++;
  return n;
}

function extractRunScripts(yamlText) {
  /** @type {string[]} */
  const scripts = [];
  const lines = yamlText.split("\n");
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    const trimmed = line.trimStart();
    if (!trimmed.startsWith("run:")) continue;

    const baseIndent = leadingSpaces(line);
    const after = trimmed.slice("run:".length).trimStart();
    if (after.startsWith("|") || after.startsWith(">")) {
      const blockIndentMin = baseIndent + 1;
      const block = [];
      for (let j = i + 1; j < lines.length; j++) {
        const next = lines[j];
        if (!next.trim()) {
          block.push("");
          continue;
        }
        if (leadingSpaces(next) < blockIndentMin) break;
        block.push(next.slice(blockIndentMin));
        i = j;
      }
      scripts.push(block.join("\n"));
      continue;
    }

    scripts.push(after);
  }
  return scripts;
}

test("workflows: node --experimental-strip-types must include TS-strip loader when running .ts entrypoints", async () => {
  const workflowsDir = path.join(repoRoot, ".github", "workflows");
  const workflowFiles = (await listFilesRecursive(workflowsDir)).filter((rel) => rel.endsWith(".yml") || rel.endsWith(".yaml"));

  /** @type {Array<{ file: string; reason: string; snippet: string }>} */
  const violations = [];
  for (const rel of workflowFiles) {
    const workflowRel = `.github/workflows/${rel}`;
    const content = await readText(workflowRel);

    for (const rawScript of extractRunScripts(content)) {
      const script = normalizeShellLineContinuations(rawScript);
      for (const line of script.split("\n")) {
        if (!line.includes("node ")) continue;
        if (!commandRunsTsWithStripTypes(line)) continue;

        const hasLoader = commandHasTsStripLoaderImport(line);
        if (hasLoader) continue;

        violations.push({
          file: workflowRel,
          reason:
            'runs a .ts entrypoint under "--experimental-strip-types" without registering the TS-strip loader via "--import .../register-ts-strip-loader.mjs"',
          snippet: line.trim(),
        });
      }
    }
  }

  assert.deepEqual(
    violations,
    [],
    `Some workflows run TS entrypoints under "--experimental-strip-types" without the TS-strip loader:\n${violations
      .map((v) => `- ${v.file}: ${v.reason}\n  ${v.snippet}`)
      .join("\n")}`,
  );
});

