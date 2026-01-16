import assert from "node:assert/strict";
import test from "node:test";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { readdir, readFile } from "node:fs/promises";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

function basename(p) {
  return p.split("/").pop() ?? p;
}

test("module boundaries: repo-root tests must not import TS sources from CJS workspaces", async () => {
  const testsDir = __dirname;
  const entries = await readdir(testsDir, { withFileTypes: true });
  const files = entries
    .filter((e) => e.isFile())
    .map((e) => e.name)
    .filter((name) => name.endsWith(".test.js") || name.endsWith(".test.ts"))
    .filter((name) => name !== basename(__filename))
    .sort();

  // Keep these as concatenated strings so this test doesn't trip itself.
  const forbidden = [
    {
      id: "net-proxy-src",
      needle: ["..", "/net-proxy/src/"].join(""),
      reason: "net-proxy is a CommonJS workspace; validate its TS sources via its own package tests (dist output).",
    },
    {
      id: "image-gateway-src",
      needle: ["..", "/services/image-gateway/src/"].join(""),
      reason:
        "image-gateway is not ESM-typed for repo-root imports; validate TS sources via its workspace tests (vitest).",
    },
  ];

  const violations = [];
  for (const name of files) {
    const fullPath = path.join(testsDir, name);
    const content = await readFile(fullPath, "utf8");
    for (const rule of forbidden) {
      if (content.includes(rule.needle)) {
        violations.push({ file: name, rule: rule.id, reason: rule.reason });
      }
    }
  }

  assert.deepEqual(violations, [], `Module boundary violations: ${JSON.stringify(violations, null, 2)}`);
});

