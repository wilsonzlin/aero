import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { commandHasTsStripLoaderImport, commandRunsTsWithStripTypes } from "./_helpers/ts_strip_loader_contract_helpers.js";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const repoRoot = path.resolve(__dirname, "..");

async function readJson(relPath) {
  const abs = path.join(repoRoot, relPath);
  const raw = await fs.readFile(abs, "utf8");
  return JSON.parse(raw);
}

async function exists(absPath) {
  try {
    await fs.stat(absPath);
    return true;
  } catch {
    return false;
  }
}

async function listWorkspacePackageJsonRelPaths() {
  /** @type {Set<string>} */
  const out = new Set(["package.json"]);

  const rootPkg = await readJson("package.json");
  const workspaces = rootPkg?.workspaces ?? [];
  assert.ok(Array.isArray(workspaces), "expected root package.json workspaces to be an array");

  for (const entry of workspaces) {
    if (typeof entry !== "string" || entry.trim() === "") continue;

    // Support the small subset of npm workspace patterns we use in this repo.
    // - Literal paths: "web", "bench", "backend/aero-gateway", ...
    // - One-segment wildcards: "packages/*"
    if (entry.endsWith("/*")) {
      const baseRel = entry.slice(0, -2);
      const baseAbs = path.join(repoRoot, baseRel);
      const dirents = await fs.readdir(baseAbs, { withFileTypes: true });
      dirents.sort((a, b) => a.name.localeCompare(b.name));
      for (const ent of dirents) {
        if (!ent.isDirectory()) continue;
        const pkgRel = `${baseRel}/${ent.name}/package.json`;
        const pkgAbs = path.join(repoRoot, pkgRel);
        if (await exists(pkgAbs)) out.add(pkgRel);
      }
      continue;
    }

    if (entry.includes("*")) {
      throw new Error(`Unsupported workspace pattern in root package.json: ${entry}`);
    }

    const pkgRel = `${entry}/package.json`;
    const pkgAbs = path.join(repoRoot, pkgRel);
    assert.ok(await exists(pkgAbs), `Workspace entry is missing package.json: ${pkgRel}`);
    out.add(pkgRel);
  }

  return [...out].sort((a, b) => a.localeCompare(b));
}

test("package.json scripts: node --experimental-strip-types running .ts entrypoints must register the TS-strip loader", async () => {
  const packageJsonFiles = await listWorkspacePackageJsonRelPaths();

  /** @type {Array<{ file: string; script: string; cmd: string }>} */
  const violations = [];
  for (const relPath of packageJsonFiles) {
    const pkg = await readJson(relPath);
    const scripts = pkg?.scripts ?? {};
    if (!scripts || typeof scripts !== "object") continue;

    for (const [name, cmd] of Object.entries(scripts)) {
      if (typeof cmd !== "string") continue;
      if (!commandRunsTsWithStripTypes(cmd)) continue;

      const hasLoader = commandHasTsStripLoaderImport(cmd);
      if (hasLoader) continue;

      violations.push({ file: relPath, script: name, cmd });
    }
  }

  assert.deepEqual(
    violations,
    [],
    `Some package.json scripts run TS entrypoints under "--experimental-strip-types" without the TS-strip loader:\n${violations
      .map((v) => `- ${v.file} [scripts.${v.script}]\n  ${v.cmd}`)
      .join("\n")}`,
  );
});

