import assert from "node:assert/strict";
import test from "node:test";
import fs from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { collectJsTsSourceFiles, findLineNumber, stripStringsAndComments } from "./_helpers/js_source_scan_helpers.js";

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

function findRemoteAddressGetterHitsInSource(source) {
  const masked = stripStringsAndComments(source);
  const hits = [];

  // `socket.remoteAddress` is a common "getter read" in Node servers. In this repo's defensive
  // model, property reads can throw under hostile/monkeypatched objects; prefer safe getters
  // (`tryGetStringProp(tryGetProp(req, "socket"), "remoteAddress")`) instead.
  const re = /(?:\.|\?\.)\s*remoteAddress\b/gmu;
  for (;;) {
    const m = re.exec(masked);
    if (!m) break;
    hits.push({ kind: ".remoteAddress", index: m.index });
  }

  return hits;
}

test("contract: avoid socket.remoteAddress direct reads in production sources", async () => {
  const files = await collectJsTsSourceFiles(repoRoot);
  const violations = [];

  for (const rel of files.sort()) {
    const abs = path.join(repoRoot, rel);
    const content = await fs.readFile(abs, "utf8");
    const hits = findRemoteAddressGetterHitsInSource(content);
    for (const hit of hits) {
      const line = findLineNumber(content, hit.index);
      violations.push({ file: rel, line, kind: hit.kind });
    }
  }

  assert.deepEqual(violations, [], `remoteAddress getter violations: ${JSON.stringify(violations, null, 2)}`);
});

