import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import test from "node:test";
import { fileURLToPath } from "node:url";

const repoRoot = fileURLToPath(new URL("..", import.meta.url));
const isWin = process.platform === "win32";

function formatSpawnFailure(cmd, args, res) {
  const joined = [cmd, ...args].join(" ");
  const extra = res.error ? `\n\nerror:\n${res.error.stack || res.error.message}` : "";
  return `command failed: ${joined} (status=${res.status})${extra}\n\nstdout:\n${res.stdout}\n\nstderr:\n${res.stderr}`;
}

test("npm run check:goldens exits 0 and does not modify tracked goldens", () => {
  const res = spawnSync("npm", ["run", "check:goldens"], {
    cwd: repoRoot,
    encoding: "utf8",
    // `npm` is a .cmd shim on Windows; spawn via the shell so this test works in
    // the CI smoke-matrix job (windows-latest).
    shell: isWin,
  });
  assert.equal(res.status, 0, formatSpawnFailure("npm", ["run", "check:goldens"], res));
});
