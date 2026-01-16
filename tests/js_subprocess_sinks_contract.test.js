import assert from "node:assert/strict";
import test from "node:test";
import fs from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { collectJsTsSourceFiles, findLineNumber, stripStringsAndComments } from "./_helpers/js_source_scan_helpers.js";

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

function findSubprocessSinks(masked) {
  const hits = [];

  const importExecRe = /\bimport\s+\{[^}]*\bexec\b[^}]*\}\s+from\s+["'](?:node:)?child_process["']/gmu;
  for (;;) {
    const m = importExecRe.exec(masked);
    if (!m) break;
    hits.push({ kind: "import{exec} child_process", index: m.index });
  }

  const importExecSyncRe = /\bimport\s+\{[^}]*\bexecSync\b[^}]*\}\s+from\s+["'](?:node:)?child_process["']/gmu;
  for (;;) {
    const m = importExecSyncRe.exec(masked);
    if (!m) break;
    hits.push({ kind: "import{execSync} child_process", index: m.index });
  }

  const requireExecRe = /\brequire\s*\(\s*["'](?:node:)?child_process["']\s*\)\s*\.\s*exec\s*\(/gmu;
  for (;;) {
    const m = requireExecRe.exec(masked);
    if (!m) break;
    hits.push({ kind: "require(child_process).exec(", index: m.index });
  }

  const requireExecSyncRe = /\brequire\s*\(\s*["'](?:node:)?child_process["']\s*\)\s*\.\s*execSync\s*\(/gmu;
  for (;;) {
    const m = requireExecSyncRe.exec(masked);
    if (!m) break;
    hits.push({ kind: "require(child_process).execSync(", index: m.index });
  }

  const shellTrueRe = /\bshell\s*:\s*true\b/gmu;
  for (;;) {
    const m = shellTrueRe.exec(masked);
    if (!m) break;
    hits.push({ kind: "shell: true", index: m.index });
  }

  return hits;
}

test("contract: no unsafe subprocess execution sinks in production sources", async () => {
  const files = await collectJsTsSourceFiles(repoRoot);

  const allowlist = new Set([
    // None today; add entries only with explicit justification.
  ]);

  const violations = [];
  for (const rel of files) {
    if (allowlist.has(rel)) continue;
    const abs = path.join(repoRoot, rel);
    const content = await fs.readFile(abs, "utf8");
    const masked = stripStringsAndComments(content);
    const hits = findSubprocessSinks(masked);
    for (const hit of hits) {
      violations.push({ file: rel, line: findLineNumber(content, hit.index), kind: hit.kind });
    }
  }

  assert.deepEqual(violations, [], `subprocess sink violations: ${JSON.stringify(violations, null, 2)}`);
});

