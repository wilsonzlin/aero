import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";
import test from "node:test";

import { collectJsTsSourceFiles, findLineNumber, stripStringsAndComments } from "./_helpers/js_source_scan_helpers.js";

const ALLOWED_DGRAM_CREATE_SOCKET = new Set([
  "backend/aero-gateway/bench/run.mjs",
  "backend/aero-gateway/src/dns/upstream.ts",
  "net-proxy/src/udpRelay.ts",
]);

function findFirstDgramCreateSocketCallIndex(maskedSource) {
  const needle = "dgram.createSocket";
  let idx = maskedSource.indexOf(needle);
  while (idx !== -1) {
    let i = idx + needle.length;
    while (i < maskedSource.length && /\s/.test(maskedSource[i])) i += 1;
    if (maskedSource[i] === "(") return idx;
    idx = maskedSource.indexOf(needle, idx + needle.length);
  }
  return -1;
}

test("js_source_scan: dgram.createSocket usage is restricted to known modules", async () => {
  const repoRoot = process.cwd();
  const sources = await collectJsTsSourceFiles(repoRoot);

  const offenders = [];
  for (const rel of sources) {
    const abs = path.join(repoRoot, rel);
    const source = await fs.readFile(abs, "utf8");
    const masked = stripStringsAndComments(source);

    const firstIdx = findFirstDgramCreateSocketCallIndex(masked);
    if (firstIdx === -1) continue;
    if (ALLOWED_DGRAM_CREATE_SOCKET.has(rel)) continue;

    offenders.push({ rel, line: findLineNumber(source, firstIdx) });
  }

  assert.equal(
    offenders.length,
    0,
    `Unexpected dgram.createSocket usage outside allowlist:\n${offenders
      .map((o) => `- ${o.rel}:${o.line}`)
      .join("\n")}`,
  );
});

