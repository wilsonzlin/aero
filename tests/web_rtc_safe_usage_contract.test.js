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

test("contract: web/net must use rtcSafe for RTCDataChannel/RTCPeerConnection close/send", async () => {
  const files = await collectJsTsSourceFiles(repoRoot, ["web/src/net"]);

  const allowlist = new Set([
    // Canonical wrappers for WebRTC close/send.
    "web/src/net/rtcSafe.ts",
  ]);

  const rules = [
    { kind: "pc.close()", re: /\bpc\s*\.\s*close\s*\(/gu },
    { kind: "dc.close()", re: /\bdc\s*\.\s*close\s*\(/gu },
    { kind: "channel.close()", re: /\bchannel\s*\.\s*close\s*\(/gu },
    { kind: "dataChannel.close()", re: /\bdataChannel\s*\.\s*close\s*\(/gu },
    { kind: "dc.send()", re: /\bdc\s*\.\s*send\s*\(/gu },
    { kind: "channel.send()", re: /\bchannel\s*\.\s*send\s*\(/gu },
    { kind: "dataChannel.send()", re: /\bdataChannel\s*\.\s*send\s*\(/gu },
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

  assert.deepEqual(violations, [], `rtcSafe usage violations: ${JSON.stringify(violations, null, 2)}`);
});
