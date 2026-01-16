import assert from "node:assert/strict";
import test from "node:test";
import fs from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { collectJsTsSourceFiles, findLineNumber } from "./_helpers/js_source_scan_helpers.js";
import { findEvalSinksInSource } from "./_helpers/eval_sink_scan_helpers.js";

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

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
    const hits = findEvalSinksInSource(content);
    for (const hit of hits) {
      const line = findLineNumber(content, hit.index);
      violations.push({ file: rel, line, kind: hit.kind });
    }
  }

  assert.deepEqual(violations, [], `eval sink violations: ${JSON.stringify(violations, null, 2)}`);
});

