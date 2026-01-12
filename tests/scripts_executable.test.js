import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

function walk(dir) {
  const entries = fs.readdirSync(dir, { withFileTypes: true });
  const out = [];
  for (const ent of entries) {
    const p = path.join(dir, ent.name);
    if (ent.isDirectory()) {
      out.push(...walk(p));
    } else if (ent.isFile()) {
      out.push(p);
    }
  }
  return out;
}

function isExecutable(mode) {
  // Any execute bit (user/group/other).
  return (mode & 0o111) !== 0;
}

test("shell scripts checked in under scripts/ are executable", { skip: process.platform === "win32" }, () => {
  const scriptRoots = [path.join(repoRoot, "scripts"), path.join(repoRoot, "deploy", "scripts")];

  const offenders = [];
  for (const root of scriptRoots) {
    if (!fs.existsSync(root)) continue;
    for (const file of walk(root)) {
      if (!file.endsWith(".sh")) continue;
      const st = fs.statSync(file);
      if (!isExecutable(st.mode)) {
        offenders.push(path.relative(repoRoot, file));
      }
    }
  }

  assert.equal(
    offenders.length,
    0,
    `Expected all scripts/**/*.sh to be executable, but found:\n${offenders.map((p) => `- ${p}`).join("\n")}`,
  );
});

