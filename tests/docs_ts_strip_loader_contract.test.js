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

function collectMarkdownFiles({ repoRoot }) {
  const out = ["README.md", "bench/README.md"];
  return Promise.all([Promise.resolve(out), listFilesRecursive(path.join(repoRoot, "docs")).then((xs) => xs.map((x) => `docs/${x}`))]).then(
    ([a, b]) => a.concat(b.filter((p) => p.endsWith(".md"))),
  );
}

test("docs: node --experimental-strip-types invocations running .ts entrypoints must register the TS-strip loader", async () => {
  const mdFiles = await collectMarkdownFiles({ repoRoot });

  /** @type {Array<{ file: string; line: number; text: string }>} */
  const violations = [];
  for (const relPath of mdFiles) {
    const raw = await readText(relPath);
    const normalized = normalizeShellLineContinuations(raw);
    const lines = normalized.split("\n");
    for (let i = 0; i < lines.length; i++) {
      const line = lines[i];
      if (!line.includes("node --experimental-strip-types")) continue;
      if (!commandRunsTsWithStripTypes(line)) continue;

      const ok = commandHasTsStripLoaderImport(line, { requiredPathFragment: "scripts/register-ts-strip-loader.mjs" });
      if (ok) continue;

      violations.push({ file: relPath, line: i + 1, text: line.trim() });
    }
  }

  assert.deepEqual(
    violations,
    [],
    `Some docs run TS entrypoints under "--experimental-strip-types" without the TS-strip loader:\n${violations
      .map((v) => `- ${v.file}:${v.line}\n  ${v.text}`)
      .join("\n")}`,
  );
});

