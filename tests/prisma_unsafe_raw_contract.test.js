import assert from "node:assert/strict";
import test from "node:test";
import fs from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { collectJsTsSourceFiles, findLineNumber, stripStringsAndComments } from "./_helpers/js_source_scan_helpers.js";

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

function findUnsafePrismaRaw(masked) {
  const hits = [];
  const patterns = [
    { kind: "queryRawUnsafe", re: /\bqueryRawUnsafe\b/gmu },
    { kind: "executeRawUnsafe", re: /\bexecuteRawUnsafe\b/gmu },
    { kind: "$queryRawUnsafe", re: /\$\s*queryRawUnsafe\b/gmu },
    { kind: "$executeRawUnsafe", re: /\$\s*executeRawUnsafe\b/gmu },
  ];

  for (const p of patterns) {
    for (;;) {
      const m = p.re.exec(masked);
      if (!m) break;
      hits.push({ kind: p.kind, index: m.index });
    }
  }
  return hits;
}

test("contract: forbid Prisma unsafe raw query APIs in production sources", async () => {
  const files = await collectJsTsSourceFiles(repoRoot);

  const violations = [];
  for (const rel of files) {
    const abs = path.join(repoRoot, rel);
    const content = await fs.readFile(abs, "utf8");
    const masked = stripStringsAndComments(content);
    for (const hit of findUnsafePrismaRaw(masked)) {
      violations.push({ file: rel, line: findLineNumber(content, hit.index), kind: hit.kind });
    }
  }

  assert.deepEqual(violations, [], `Prisma unsafe raw API violations: ${JSON.stringify(violations, null, 2)}`);
});

