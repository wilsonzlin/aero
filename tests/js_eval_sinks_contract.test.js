import assert from "node:assert/strict";
import test from "node:test";
import fs from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { collectJsTsSourceFiles, findLineNumber, stripStringsAndComments } from "./_helpers/js_source_scan_helpers.js";

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

function findEvalSinks(content) {
  const hits = [];

  // Direct eval() call (not obj.eval()).
  const directEvalRe = /(^|[^.\w$])eval\s*\(/gmu;
  for (;;) {
    const m = directEvalRe.exec(content);
    if (!m) break;
    hits.push({ kind: "eval", index: m.index });
  }

  // Explicit global eval references (these are still eval sinks).
  const globalEvalRe = /\b(globalThis|window|self)\s*\.\s*eval\s*\(/gmu;
  for (;;) {
    const m = globalEvalRe.exec(content);
    if (!m) break;
    hits.push({ kind: "globalEval", index: m.index });
  }

  // Function constructor.
  const newFunctionRe = /\bnew\s+Function\s*\(/gmu;
  for (;;) {
    const m = newFunctionRe.exec(content);
    if (!m) break;
    hits.push({ kind: "newFunction", index: m.index });
  }

  // Function() call is also an eval sink (same as new Function()).
  // Avoid double-counting `new Function(` as both.
  const functionCallRe = /\bFunction\s*\(/gmu;
  for (;;) {
    const m = functionCallRe.exec(content);
    if (!m) break;
    if (/\bnew\s+Function\s*\(/gmu.test(content.slice(Math.max(0, m.index - 16), m.index + 16))) continue;
    hits.push({ kind: "Function", index: m.index });
  }

  return hits;
}

test("contract: no JS eval sinks in production sources", async () => {
  const files = await collectJsTsSourceFiles(repoRoot);

  const allowlist = new Set([
    // Intentional CSP gate fixture: contains eval() to prove CSP blocks it.
    "web/public/assets/security_headers_worker.js",
  ]);

  const violations = [];
  for (const rel of files.sort()) {
    if (allowlist.has(rel)) continue;
    const abs = path.join(repoRoot, rel);
    const content = await fs.readFile(abs, "utf8");
    const masked = stripStringsAndComments(content);
    const hits = findEvalSinks(masked);
    for (const hit of hits) {
      const line = findLineNumber(content, hit.index);
      violations.push({ file: rel, line, kind: hit.kind });
    }
  }

  assert.deepEqual(violations, [], `eval sink violations: ${JSON.stringify(violations, null, 2)}`);
});

