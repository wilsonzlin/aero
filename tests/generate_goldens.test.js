import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import test from "node:test";
import { fileURLToPath } from "node:url";

const repoRoot = fileURLToPath(new URL("..", import.meta.url));

test("npm run generate:goldens exits 0 and does not modify tracked goldens", () => {
  const res = spawnSync("npm", ["run", "generate:goldens"], {
    cwd: repoRoot,
    encoding: "utf8",
  });
  assert.equal(
    res.status,
    0,
    `generate:goldens failed (status=${res.status})\n\nstdout:\n${res.stdout}\n\nstderr:\n${res.stderr}`,
  );

  const diff = spawnSync("git", ["diff", "--exit-code", "--", "tests/golden"], {
    cwd: repoRoot,
    encoding: "utf8",
  });
  assert.equal(
    diff.status,
    0,
    `generate:goldens produced a diff in tests/golden\n\nstdout:\n${diff.stdout}\n\nstderr:\n${diff.stderr}`,
  );

  // `git diff` does not report untracked files. Ensure the generator did not
  // produce any new goldens that were forgotten in the commit.
  const status = spawnSync("git", ["status", "--porcelain", "--", "tests/golden"], {
    cwd: repoRoot,
    encoding: "utf8",
  });
  assert.equal(status.status, 0, `git status failed\n\nstdout:\n${status.stdout}\n\nstderr:\n${status.stderr}`);
  assert.equal(
    status.stdout.trim(),
    "",
    `generate:goldens produced uncommitted changes in tests/golden\n\n${status.stdout}\n\nRun \`npm run generate:goldens\` and commit the updated PNGs under tests/golden/.`,
  );
});
