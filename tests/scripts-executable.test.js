import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const repoRoot = fileURLToPath(new URL("..", import.meta.url));

function gitFileMode(relPath) {
  const out = execFileSync("git", ["ls-files", "--stage", "--", relPath], {
    cwd: repoRoot,
    encoding: "utf8",
  }).trim();
  if (!out) return null;
  // Format: "<mode> <blob> <stage>\t<path>"
  return out.split(/\s+/, 1)[0];
}

function trackedShellScripts() {
  // Enumerate via git so the test doesn't accidentally recurse into large
  // untracked dirs (node_modules/, target/, etc.) in local checkouts.
  return execFileSync("git", ["ls-files"], {
    cwd: repoRoot,
    encoding: "utf8",
  })
    .split("\n")
    .map((line) => line.trim())
    .filter((line) => line.length > 0 && line.endsWith(".sh"));
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
  //   (cd infra/local-object-store && ./verify.sh)
  // Keep the character class conservative; `.sh` is an unambiguous terminator.
  //
  // Note: avoid matching inside `../foo.sh` or `../../foo.sh` by requiring the
  // `./` not be preceded by another dot.
  const re = /(^|[^.])(\\.\/[0-9A-Za-z_./-]+\.sh)/gm;

  const scripts = new Set();
  const missing = [];
  for (const relDocPath of markdownFiles) {
    const absDocPath = path.join(repoRoot, relDocPath);
    const content = fs.readFileSync(absDocPath, "utf8");
    for (const m of content.matchAll(re)) {
      const match = m[2];
      const relPathNoDot = match.replace(/^\.\//, "");
      // First try repo-root relative (common case in root-level commands).
      const absRepoRoot = path.join(repoRoot, relPathNoDot);
      if (fs.existsSync(absRepoRoot)) {
        scripts.add(path.relative(repoRoot, absRepoRoot));
        continue;
      }

      // Then try doc-relative (common in README.md files that assume `cd` into
      // the directory first, like `infra/local-object-store/README.md`).
      const absDocRelative = path.join(path.dirname(absDocPath), relPathNoDot);
      if (fs.existsSync(absDocRelative)) {
        scripts.add(path.relative(repoRoot, absDocRelative));
        continue;
      }

      missing.push(`${relDocPath}: ${match}`);
    }
  }
  assert.deepEqual(
    missing,
    [],
    `some docs reference shell scripts that do not exist (checked repo-root and doc-relative paths)`,
  );
  return scripts;
}

function scriptsToCheck() {
  const scripts = new Set();

  // Ensure every tracked shell script stays executable (CI/workflows rely on this).
  for (const relPath of trackedShellScripts()) scripts.add(relPath);

  // Ensure any docs invoking `./...*.sh` refer to an existing executable file.
  for (const docScript of scriptsReferencedByDocs()) scripts.add(docScript);

  return [...scripts].sort();
}

test("scripts referenced as ./scripts/*.sh are executable", { skip: process.platform === "win32" }, () => {
  for (const relPath of scriptsToCheck()) {
    const absPath = path.join(repoRoot, relPath);
    assert.ok(fs.existsSync(absPath), `${relPath} is missing`);

    const { mode: fsMode } = fs.statSync(absPath);
    // Any executable bit (user/group/other) is good enough.
    assert.ok((fsMode & 0o111) !== 0, `${relPath} is not executable (expected chmod +x / git mode 100755)`);

    const modeInGit = gitFileMode(relPath);
    if (modeInGit !== null) {
      assert.equal(modeInGit, "100755", `${relPath} is not executable in git (expected mode 100755)`);
    }

    // Scripts invoked directly (via `./foo.sh`) must have a shebang, otherwise
    // Linux will fail with `Exec format error`.
    const firstLine = fs.readFileSync(absPath, "utf8").split(/\r?\n/, 1)[0];
    assert.ok(firstLine.startsWith("#!"), `${relPath} is missing a shebang (expected first line to start with '#!')`);
  }
});

test("safe-run.sh can execute a trivial command (Linux)", { skip: process.platform !== "linux" }, () => {
  execFileSync(path.join(repoRoot, "scripts/safe-run.sh"), ["true"], {
    cwd: repoRoot,
    stdio: "ignore",
  });
});
