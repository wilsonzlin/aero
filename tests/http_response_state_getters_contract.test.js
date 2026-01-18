import assert from "node:assert/strict";
import test from "node:test";
import fs from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { collectJsTsSourceFiles, findLineNumber, stripStringsAndComments } from "./_helpers/js_source_scan_helpers.js";

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

function findResponseStateGetterHitsInSource(source) {
  const masked = stripStringsAndComments(source);
  const hits = [];

  // These response state getters are frequently used in Node server error paths to decide whether
  // to write a fallback response. In hostile/monkeypatched environments, property reads can
  // synchronously throw; prefer "best-effort write + destroy on failure" patterns instead.
  const patterns = [
    { re: /(?:\.|\?\.)\s*headersSent\b/gmu, kind: ".headersSent" },
    { re: /(?:\.|\?\.)\s*writableEnded\b/gmu, kind: ".writableEnded" },
  ];

  for (const { re, kind } of patterns) {
    for (;;) {
      const m = re.exec(masked);
      if (!m) break;
      hits.push({ kind, index: m.index });
    }
  }

  return hits;
}

test("contract: avoid res.headersSent/res.writableEnded in production sources", async () => {
  const files = await collectJsTsSourceFiles(repoRoot);
  const violations = [];

  for (const rel of files.sort()) {
    const abs = path.join(repoRoot, rel);
    const content = await fs.readFile(abs, "utf8");
    const hits = findResponseStateGetterHitsInSource(content);
    for (const hit of hits) {
      const line = findLineNumber(content, hit.index);
      violations.push({ file: rel, line, kind: hit.kind });
    }
  }

  assert.deepEqual(
    violations,
    [],
    `Response state getter violations (prefer best-effort write/destroy patterns): ${JSON.stringify(violations, null, 2)}`,
  );
});

