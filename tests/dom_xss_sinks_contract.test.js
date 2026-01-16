import assert from "node:assert/strict";
import test from "node:test";
import fs from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { collectJsTsSourceFiles, findLineNumber, stripStringsAndComments } from "./_helpers/js_source_scan_helpers.js";

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

function findXssSinks(masked) {
  const hits = [];

  // React sink.
  const dangerouslyRe = /\bdangerouslySetInnerHTML\b/gmu;
  for (;;) {
    const m = dangerouslyRe.exec(masked);
    if (!m) break;
    hits.push({ kind: "dangerouslySetInnerHTML", index: m.index });
  }

  // DOM sinks.
  const innerRe = /\.\s*innerHTML\b/gmu;
  for (;;) {
    const m = innerRe.exec(masked);
    if (!m) break;
    hits.push({ kind: ".innerHTML", index: m.index });
  }

  const outerRe = /\.\s*outerHTML\b/gmu;
  for (;;) {
    const m = outerRe.exec(masked);
    if (!m) break;
    hits.push({ kind: ".outerHTML", index: m.index });
  }

  const insertRe = /\.\s*insertAdjacentHTML\b/gmu;
  for (;;) {
    const m = insertRe.exec(masked);
    if (!m) break;
    hits.push({ kind: ".insertAdjacentHTML", index: m.index });
  }

  // document.write / writeln.
  const writeRe = /\bdocument\s*\.\s*writeln?\s*\(/gmu;
  for (;;) {
    const m = writeRe.exec(masked);
    if (!m) break;
    hits.push({ kind: "document.write", index: m.index });
  }

  // Range.createContextualFragment (HTML injection via parsing).
  const fragmentRe = /\.\s*createContextualFragment\b/gmu;
  for (;;) {
    const m = fragmentRe.exec(masked);
    if (!m) break;
    hits.push({ kind: ".createContextualFragment", index: m.index });
  }

  return hits;
}

test("contract: no DOM HTML injection sinks in production sources", async () => {
  const files = await collectJsTsSourceFiles(repoRoot);

  const violations = [];
  for (const rel of files.sort()) {
    const abs = path.join(repoRoot, rel);
    const content = await fs.readFile(abs, "utf8");
    const masked = stripStringsAndComments(content);
    const hits = findXssSinks(masked);
    for (const hit of hits) {
      violations.push({ file: rel, line: findLineNumber(content, hit.index), kind: hit.kind });
    }
  }

  assert.deepEqual(violations, [], `DOM XSS sink violations: ${JSON.stringify(violations, null, 2)}`);
});

