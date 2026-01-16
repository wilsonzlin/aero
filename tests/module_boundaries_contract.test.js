import assert from "node:assert/strict";
import test from "node:test";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { readdir, readFile } from "node:fs/promises";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const repoRoot = path.resolve(__dirname, "..");

function basename(p) {
  return p.split("/").pop() ?? p;
}

async function collectTestFiles(dir) {
  const out = [];
  const entries = await readdir(dir, { withFileTypes: true });
  for (const entry of entries) {
    const fullPath = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      out.push(...(await collectTestFiles(fullPath)));
      continue;
    }
    if (!entry.isFile()) continue;
    if (!entry.name.endsWith(".test.js") && !entry.name.endsWith(".test.ts")) continue;
    out.push(fullPath);
  }
  return out;
}

test("module boundaries: repo-root tests must not import TS sources from CJS workspaces", async () => {
  const roots = [
    "tests",
    "backend/aero-gateway/test",
    "bench",
    "tools/perf/tests",
    "tools/range-harness/test",
    "packages/aero-stats/test",
    "web/test",
    "emulator/protocol/tests",
  ].map((p) => path.join(repoRoot, p));

  const all = [];
  for (const dir of roots) {
    try {
      all.push(...(await collectTestFiles(dir)));
    } catch {
      // Ignore missing directories (workspaces may be pruned in some environments).
    }
  }

  const files = all
    .filter((p) => basename(p) !== basename(__filename))
    .map((p) => path.relative(repoRoot, p))
    .sort();

  // Keep these as concatenated strings so this test doesn't trip itself.
  const forbidden = [
    {
      id: "net-proxy-src",
      needle: ["/net-proxy", "/src/"].join(""),
      reason: "net-proxy is a CommonJS workspace; validate its TS sources via its own package tests (dist output).",
    },
    {
      id: "image-gateway-src",
      needle: ["/services", "/image-gateway/src/"].join(""),
      reason:
        "image-gateway is not ESM-typed for repo-root imports; validate TS sources via its workspace tests (vitest).",
    },
  ];

  const violations = [];
  for (const name of files) {
    const fullPath = path.join(repoRoot, name);
    const content = await readFile(fullPath, "utf8");
    for (const rule of forbidden) {
      if (content.includes(rule.needle)) {
        violations.push({ file: name, rule: rule.id, reason: rule.reason });
      }
    }
  }

  assert.deepEqual(violations, [], `Module boundary violations: ${JSON.stringify(violations, null, 2)}`);
});

