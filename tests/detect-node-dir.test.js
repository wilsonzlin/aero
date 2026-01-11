import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const sourceResolverPath = fileURLToPath(new URL("../scripts/ci/detect-node-dir.mjs", import.meta.url));

function writeJson(filePath, value) {
  fs.mkdirSync(path.dirname(filePath), { recursive: true });
  fs.writeFileSync(filePath, `${JSON.stringify(value, null, 2)}\n`);
}

function parseKeyVal(stdout) {
  const out = {};
  for (const line of stdout.split(/\r?\n/u)) {
    if (!line.trim()) continue;
    const idx = line.indexOf("=");
    if (idx === -1) continue;
    out[line.slice(0, idx)] = line.slice(idx + 1);
  }
  return out;
}

function setupTempRepo() {
  const repoRoot = fs.mkdtempSync(path.join(os.tmpdir(), "aero-node-dir-"));
  const resolverDest = path.join(repoRoot, "scripts/ci/detect-node-dir.mjs");
  fs.mkdirSync(path.dirname(resolverDest), { recursive: true });
  fs.copyFileSync(sourceResolverPath, resolverDest);
  return { repoRoot, resolverPath: resolverDest };
}

function runResolver({ repoRoot, resolverPath }, extraArgs = []) {
  const stdout = execFileSync(process.execPath, [resolverPath, "--require-lockfile", ...extraArgs], {
    encoding: "utf8",
    cwd: repoRoot,
    env: {
      ...process.env,
      AERO_NODE_DIR: "",
      AERO_WEB_DIR: "",
    },
    stdio: ["ignore", "pipe", "pipe"],
  });
  return parseKeyVal(stdout);
}

test("detect-node-dir: prefers repo root when root + web exist (override-able)", () => {
  const temp = setupTempRepo();
  try {
    writeJson(path.join(temp.repoRoot, "package.json"), { name: "root", version: "1.0.0" });
    writeJson(path.join(temp.repoRoot, "package-lock.json"), { lockfileVersion: 3 });
    writeJson(path.join(temp.repoRoot, "web/package.json"), { name: "web", version: "0.0.0" });
    writeJson(path.join(temp.repoRoot, "web/package-lock.json"), { lockfileVersion: 3 });

    const detected = runResolver(temp);
    assert.equal(detected.dir, ".");
    assert.equal(detected.lockfile, "package-lock.json");
    assert.equal(detected.package_name, "root");

    const overridden = runResolver(temp, ["--node-dir", "web"]);
    assert.equal(overridden.dir, "web");
    assert.equal(overridden.lockfile, "web/package-lock.json");
    assert.equal(overridden.package_name, "web");
  } finally {
    fs.rmSync(temp.repoRoot, { recursive: true, force: true });
  }
});

test("detect-node-dir: falls back to web when repo root has no package.json", () => {
  const temp = setupTempRepo();
  try {
    writeJson(path.join(temp.repoRoot, "web/package.json"), { name: "web", version: "0.0.0" });
    writeJson(path.join(temp.repoRoot, "web/package-lock.json"), { lockfileVersion: 3 });

    const detected = runResolver(temp);
    assert.equal(detected.dir, "web");
    assert.equal(detected.lockfile, "web/package-lock.json");
  } finally {
    fs.rmSync(temp.repoRoot, { recursive: true, force: true });
  }
});
