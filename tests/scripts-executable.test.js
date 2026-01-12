import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const repoRoot = fileURLToPath(new URL("..", import.meta.url));
const scriptsRoot = path.join(repoRoot, "scripts");

function listShellScripts(dir) {
  const out = [];
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const absPath = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      out.push(...listShellScripts(absPath));
      continue;
    }
    if (entry.isFile() && entry.name.endsWith(".sh")) {
      out.push(absPath);
    }
  }
  return out;
}

function scriptsReferencedByDocs() {
  // Only scan tracked Markdown files to avoid walking huge untracked dirs like
  // node_modules/ or target/ (which may exist in CI/local checkouts).
  const markdownFiles = execFileSync("git", ["ls-files"], {
    cwd: repoRoot,
    encoding: "utf8",
  })
    .split("\n")
    .map((line) => line.trim())
    .filter((line) => line.length > 0 && line.endsWith(".md"));

  // Match docs that invoke a shell script directly, e.g.:
  //   ./scripts/safe-run.sh cargo test --locked
  // Keep the character class conservative; `.sh` is an unambiguous terminator.
  const re = /\.\/scripts\/[0-9A-Za-z_./-]+\.sh/g;

  const scripts = new Set();
  for (const relDocPath of markdownFiles) {
    const absDocPath = path.join(repoRoot, relDocPath);
    const content = fs.readFileSync(absDocPath, "utf8");
    const matches = content.match(re) ?? [];
    for (const match of matches) {
      scripts.add(match.replace(/^\.\//, ""));
    }
  }
  return scripts;
}

function scriptsToCheck() {
  const scripts = new Set();

  // Ensure every script living under scripts/ stays executable.
  for (const absPath of listShellScripts(scriptsRoot)) {
    scripts.add(path.relative(repoRoot, absPath));
  }

  // Ensure any docs invoking ./scripts/... also refer to an existing executable file.
  for (const docScript of scriptsReferencedByDocs()) scripts.add(docScript);

  return [...scripts].sort();
}

test("scripts referenced as ./scripts/*.sh are executable", { skip: process.platform === "win32" }, () => {
  assert.ok(fs.existsSync(scriptsRoot), "scripts/ directory is missing");

  for (const relPath of scriptsToCheck()) {
    const absPath = path.join(repoRoot, relPath);
    assert.ok(fs.existsSync(absPath), `${relPath} is missing`);

    const { mode } = fs.statSync(absPath);
    // Any executable bit (user/group/other) is good enough.
    assert.ok((mode & 0o111) !== 0, `${relPath} is not executable (expected chmod +x / git mode 100755)`);
  }
});

test("safe-run.sh can execute a trivial command (Linux)", { skip: process.platform !== "linux" }, () => {
  execFileSync(path.join(repoRoot, "scripts/safe-run.sh"), ["true"], {
    cwd: repoRoot,
    stdio: "ignore",
  });
});
