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
  //
  // Note: We intentionally keep this list explicit (instead of auto-deriving it from workspace
  // package.json "type") because:
  // - some typeless workspaces contain ESM `.js` sources that are safe to import from repo-root tests
  // - the concrete failure mode we want to prevent is importing **TypeScript sources** from
  //   CommonJS/typeless packages where Node's module format inference can break or warn
  const forbidden = [
    {
      id: "net-proxy-src",
      pattern: new RegExp([String.raw`/net-proxy`, String.raw`/src/`, String.raw`.*\.(ts|js)\b`].join("")),
      reason:
        "net-proxy is a CommonJS workspace; validate TS sources via its own package tests (built dist output), not repo-root imports",
    },
    {
      id: "image-gateway-src",
      pattern: new RegExp(
        [String.raw`/services`, String.raw`/image-gateway/src/`, String.raw`.*\.(ts|js)\b`].join(""),
      ),
      reason:
        "image-gateway is not ESM-typed for repo-root TS source imports; validate via its workspace tests (vitest)",
    },
  ];

  const violations = [];
  for (const name of files) {
    const fullPath = path.join(repoRoot, name);
    const content = await readFile(fullPath, "utf8");
    for (const rule of forbidden) {
      if (rule.pattern.test(content)) {
        violations.push({ file: name, rule: rule.id, reason: rule.reason });
      }
    }
  }

  assert.deepEqual(violations, [], `Module boundary violations: ${JSON.stringify(violations, null, 2)}`);
});

