import assert from "node:assert/strict";
import test from "node:test";
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import path from "node:path";

import { listFilesRecursive } from "./_helpers/fs_walk.js";

function normalizeDts(text) {
  // Keep this strict enough to catch real drift, but ignore trivial whitespace noise.
  const normalizedNewlines = text.replace(/\r\n?/g, "\n");
  const lines = normalizedNewlines.split("\n").map((line) => line.replace(/[ \t]+$/g, ""));
  // Avoid false negatives from a missing trailing newline.
  return `${lines.join("\n").trimEnd()}\n`;
}

async function assertDtsPairEqual(relA, relB) {
  const here = path.dirname(fileURLToPath(import.meta.url));
  const root = path.resolve(here, "..");
  const aPath = path.resolve(root, relA);
  const bPath = path.resolve(root, relB);

  const [aRaw, bRaw] = await Promise.all([readFile(aPath, "utf8"), readFile(bPath, "utf8")]);
  const a = normalizeDts(aRaw);
  const b = normalizeDts(bRaw);

  assert.equal(
    a,
    b,
    `Expected TypeScript stub parity:\n- ${relA}\n- ${relB}\n\nKeep ESM and CJS .d.ts stubs identical (modulo whitespace).`,
  );
}

test("d.ts stubs: ESM/CJS parity for src/**/(*.cjs.d.ts, *.d.ts) pairs", async () => {
  const here = path.dirname(fileURLToPath(import.meta.url));
  const root = path.resolve(here, "..");
  const srcDir = path.resolve(root, "src");

  const relFiles = await listFilesRecursive(srcDir);
  const relFileSet = new Set(relFiles);
  const cjsDts = relFiles.filter((rel) => rel.endsWith(".cjs.d.ts")).sort();

  assert.ok(cjsDts.length > 0, "Expected at least one src/**/*.cjs.d.ts stub");

  for (const cjsDtsRel of cjsDts) {
    const esmDtsRel = cjsDtsRel.replace(/\.cjs\.d\.ts$/, ".d.ts");
    assert.ok(esmDtsRel !== cjsDtsRel, "Unexpected .cjs.d.ts filename shape");

    // Fail with a clear message if the ESM pair is missing.
    assert.ok(
      relFileSet.has(esmDtsRel),
      `Missing ESM .d.ts stub for:\n- src/${cjsDtsRel}\nExpected:\n- src/${esmDtsRel}`,
    );

    await assertDtsPairEqual(`src/${esmDtsRel}`, `src/${cjsDtsRel}`);
  }
});

