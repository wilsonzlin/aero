import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";
import test from "node:test";

import { collectJsTsSourceFiles, findLineNumber, stripStringsAndComments } from "./_helpers/js_source_scan_helpers.js";

function findFirstOptionalUnrefCallIndex(maskedSource) {
  return maskedSource.indexOf(".unref?.(");
}

test("js_source_scan: src/ must not use .unref?.() (use unrefBestEffort)", async () => {
  const repoRoot = process.cwd();
  const sources = await collectJsTsSourceFiles(repoRoot, ["src"]);

  const offenders = [];
  for (const rel of sources) {
    const abs = path.join(repoRoot, rel);
    const source = await fs.readFile(abs, "utf8");
    const masked = stripStringsAndComments(source);

    const idx = findFirstOptionalUnrefCallIndex(masked);
    if (idx === -1) continue;

    offenders.push({ rel, line: findLineNumber(source, idx) });
  }

  assert.equal(
    offenders.length,
    0,
    `Unexpected .unref?.() usage under src/ (use unrefBestEffort):\n${offenders
      .map((o) => `- ${o.rel}:${o.line}`)
      .join("\n")}`,
  );
});

