import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";
import test from "node:test";

import { collectJsTsSourceFiles, findLineNumber, stripStringsAndComments } from "./_helpers/js_source_scan_helpers.js";

function findFirstDirectUnrefCallIndex(maskedSource) {
  const needle = ".unref";
  let idx = maskedSource.indexOf(needle);
  while (idx !== -1) {
    let i = idx + needle.length;
    while (i < maskedSource.length && /\s/.test(maskedSource[i])) i += 1;

    // Allow optional chaining (`.unref?.()`); we only forbid direct calls.
    if (maskedSource[i] === "?") {
      idx = maskedSource.indexOf(needle, idx + needle.length);
      continue;
    }

    if (maskedSource[i] === "(") return idx;
    idx = maskedSource.indexOf(needle, idx + needle.length);
  }
  return -1;
}

test("js_source_scan: production sources must not call .unref() directly", async () => {
  const repoRoot = process.cwd();
  const sources = await collectJsTsSourceFiles(repoRoot);

  const offenders = [];
  for (const rel of sources) {
    const abs = path.join(repoRoot, rel);
    const source = await fs.readFile(abs, "utf8");
    const masked = stripStringsAndComments(source);

    const idx = findFirstDirectUnrefCallIndex(masked);
    if (idx === -1) continue;

    offenders.push({ rel, line: findLineNumber(source, idx) });
  }

  assert.equal(
    offenders.length,
    0,
    `Unexpected .unref() usage in production sources (prefer \`.unref?.()\`):\n${offenders
      .map((o) => `- ${o.rel}:${o.line}`)
      .join("\n")}`,
  );
});

