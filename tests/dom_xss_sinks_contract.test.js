import assert from "node:assert/strict";
import test from "node:test";
import fs from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { collectJsTsSourceFiles, findLineNumber } from "./_helpers/js_source_scan_helpers.js";
import { findDomXssSinksInSource } from "./_helpers/dom_xss_sink_scan_helpers.js";

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

test("contract: no DOM HTML injection sinks in production sources", async () => {
  const files = await collectJsTsSourceFiles(repoRoot);

  const violations = [];
  for (const rel of files.sort()) {
    const abs = path.join(repoRoot, rel);
    const content = await fs.readFile(abs, "utf8");
    const hits = findDomXssSinksInSource(content);
    for (const hit of hits) {
      violations.push({ file: rel, line: findLineNumber(content, hit.index), kind: hit.kind });
    }
  }

  assert.deepEqual(violations, [], `DOM XSS sink violations: ${JSON.stringify(violations, null, 2)}`);
});

