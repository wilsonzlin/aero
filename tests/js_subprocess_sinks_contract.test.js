import assert from "node:assert/strict";
import test from "node:test";
import fs from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { collectJsTsSourceFiles, findLineNumber } from "./_helpers/js_source_scan_helpers.js";
import { findSubprocessSinksInSource } from "./_helpers/subprocess_sink_scan_helpers.js";

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

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
    const hits = findSubprocessSinksInSource(content);
    for (const hit of hits) {
      violations.push({ file: rel, line: findLineNumber(content, hit.index), kind: hit.kind });
    }
  }

  assert.deepEqual(violations, [], `subprocess sink violations: ${JSON.stringify(violations, null, 2)}`);
});

