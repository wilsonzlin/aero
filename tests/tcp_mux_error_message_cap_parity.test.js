import test from "node:test";
import assert from "node:assert/strict";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { readFile } from "node:fs/promises";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const repoRoot = path.resolve(__dirname, "..");

const EXPECTED_CAP = 1024;
const FILES = [
  "backend/aero-gateway/src/protocol/tcpMux.ts",
  "net-proxy/src/tcpMuxProtocol.ts",
  "tools/net-proxy-server/src/protocol.js",
  "tools/net-proxy-server/src/server.js",
  "web/src/net/tcpMuxProxy.ts",
].map((p) => path.join(repoRoot, p));

function extractCap(source) {
  // Keep this intentionally simple: all implementations define a numeric constant.
  const re = /\bMAX_TCP_MUX_ERROR_MESSAGE_BYTES\b\s*=\s*(\d+)\b/g;
  let found = null;
  for (;;) {
    const m = re.exec(source);
    if (!m) break;
    if (found !== null) {
      throw new Error("Found multiple MAX_TCP_MUX_ERROR_MESSAGE_BYTES assignments");
    }
    found = Number(m[1]);
  }
  if (found === null) throw new Error("Missing MAX_TCP_MUX_ERROR_MESSAGE_BYTES assignment");
  return found;
}

test("tcp-mux error message byte caps are consistent", async () => {
  const results = [];
  for (const file of FILES) {
    const src = await readFile(file, "utf8");
    const cap = extractCap(src);
    results.push({ file: path.relative(repoRoot, file), cap });
  }

  const bad = results.filter((r) => r.cap !== EXPECTED_CAP);
  assert.deepEqual(
    bad,
    [],
    `tcp-mux error message cap mismatch (expected ${EXPECTED_CAP}): ${JSON.stringify(results, null, 2)}`,
  );
});

