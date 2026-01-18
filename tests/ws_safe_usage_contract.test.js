import assert from "node:assert/strict";
import test from "node:test";
import fs from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { collectJsTsSourceFiles, findLineNumber, stripStringsAndComments } from "./_helpers/js_source_scan_helpers.js";

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

function findMatches(source, re) {
  const out = [];
  re.lastIndex = 0;
  for (;;) {
    const m = re.exec(source);
    if (!m) break;
    out.push({ index: m.index });
    if (m.index === re.lastIndex) re.lastIndex++;
  }
  return out;
}

test("contract: node ws code must not call ws.send/ws.close directly", async () => {
  const files = await collectJsTsSourceFiles(repoRoot, [
    "server/src",
    "net-proxy/src",
    "tools/net-proxy-server/src",
    "backend/aero-gateway/src",
  ]);

  const allowlist = new Set([
    // Canonical wrappers.
    "src/ws_safe.js",
    "scripts/_shared/ws_safe.js",
    "net-proxy/src/wsClose.ts",
  ]);

  const rules = [
    { kind: "ws.send()", re: /\bws\s*\.\s*send\s*\(/gu },
    { kind: "ws.close()", re: /\bws\s*\.\s*close\s*\(/gu },
    { kind: "websocket.send()", re: /\bwebsocket\s*\.\s*send\s*\(/gu },
    { kind: "websocket.close()", re: /\bwebsocket\s*\.\s*close\s*\(/gu },
  ];

  const violations = [];
  for (const rel of files) {
    if (allowlist.has(rel)) continue;
    const abs = path.join(repoRoot, rel);
    const content = await fs.readFile(abs, "utf8");
    const masked = stripStringsAndComments(content);

    for (const rule of rules) {
      const hits = findMatches(masked, rule.re);
      for (const hit of hits) {
        violations.push({ file: rel, line: findLineNumber(content, hit.index), kind: rule.kind });
      }
    }
  }

  assert.deepEqual(violations, [], `ws_safe usage violations: ${JSON.stringify(violations, null, 2)}`);
});
