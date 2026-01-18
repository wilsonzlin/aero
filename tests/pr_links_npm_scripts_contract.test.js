import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const repoRoot = fileURLToPath(new URL("..", import.meta.url));

function normalizeScript(s) {
  return String(s || "")
    .trim()
    .replaceAll(/\s+/g, " ");
}

test("package.json: pr:url and pr:links are cross-platform and use --actions (no env assignment)", () => {
  const pkg = JSON.parse(readFileSync(path.join(repoRoot, "package.json"), "utf8"));
  assert.ok(pkg && typeof pkg === "object");
  assert.ok(pkg.scripts && typeof pkg.scripts === "object");

  const prUrl = normalizeScript(pkg.scripts["pr:url"]);
  const prLinks = normalizeScript(pkg.scripts["pr:links"]);

  assert.equal(prUrl, "node scripts/print-pr-url.mjs");
  assert.equal(prLinks, "node scripts/print-pr-url.mjs --actions");

  // Ensure we never regress to a POSIX-only env var assignment in npm scripts.
  assert.ok(!prLinks.includes("="), `expected pr:links not to include env var assignment, got: ${prLinks}`);
});

