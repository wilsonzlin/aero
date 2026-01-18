import assert from "node:assert/strict";
import test from "node:test";
import fs from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { collectJsTsSourceFiles, findLineNumber, stripStringsAndComments } from "./_helpers/js_source_scan_helpers.js";

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

const WEB_NET_ROOT = "web/src/net";
const ALLOWED_STATE_READERS = new Set([
  // Centralized, throw-safe accessors live here.
  "web/src/net/wsSafe.ts",
  "web/src/net/rtcSafe.ts",
]);

function findStateGetterHitsInSource(source) {
  const masked = stripStringsAndComments(source);
  const hits = [];

  // In this repo's hostile/monkeypatched-object model, property reads can synchronously throw.
  // These transport state getters are commonly read from within event callbacks / ticks; keep
  // direct reads localized to `wsSafe`/`rtcSafe` and use throw-safe helpers elsewhere.
  const patterns = [
    { re: /(?:\.|\?\.)\s*readyState\b/gmu, kind: ".readyState" },
    { re: /(?:\.|\?\.)\s*bufferedAmount\b/gmu, kind: ".bufferedAmount" },
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

test("contract(web): avoid direct readyState/bufferedAmount reads in web/src/net", async () => {
  const files = await collectJsTsSourceFiles(repoRoot, [WEB_NET_ROOT]);
  const violations = [];

  for (const rel of files.sort()) {
    if (ALLOWED_STATE_READERS.has(rel)) continue;
    const abs = path.join(repoRoot, rel);
    const content = await fs.readFile(abs, "utf8");
    const hits = findStateGetterHitsInSource(content);
    for (const hit of hits) {
      violations.push({
        file: rel,
        line: findLineNumber(content, hit.index),
        kind: hit.kind,
      });
    }
  }

  assert.deepEqual(
    violations,
    [],
    `web/src/net state-getter violations (use wsSafe/rtcSafe helpers): ${JSON.stringify(violations, null, 2)}`,
  );
});

