#!/usr/bin/env node
import fs from "node:fs";
import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

function rel(p) {
  const cwd = process.cwd();
  const abs = path.resolve(p);
  return path.relative(cwd, abs) || ".";
}

function stripQuotes(value) {
  const trimmed = value.trim();
  if (
    (trimmed.startsWith('"') && trimmed.endsWith('"')) ||
    (trimmed.startsWith("'") && trimmed.endsWith("'"))
  ) {
    return trimmed.slice(1, -1);
  }
  return trimmed;
}

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(scriptDir, "..", "..");
const workflowsDir = path.join(repoRoot, ".github", "workflows");
const nvmrcPath = path.join(repoRoot, ".nvmrc");

let pinnedNodeRaw = null;
try {
  pinnedNodeRaw = fs.readFileSync(nvmrcPath, "utf8");
} catch (err) {
  console.error(`error: missing .nvmrc (expected at ${rel(nvmrcPath)})`);
  console.error("This repo pins its canonical Node.js version via a checked-in .nvmrc file.");
  process.exit(1);
}

const pinnedNode = pinnedNodeRaw.trim().replace(/^v/, "");
if (!/^\d+\.\d+\.\d+$/.test(pinnedNode)) {
  console.error(`error: invalid .nvmrc value: ${JSON.stringify(pinnedNodeRaw.trim())}`);
  console.error("Expected an exact version like \"22.11.0\" (major.minor.patch).");
  process.exit(1);
}

const pinnedMajor = Number(pinnedNode.split(".")[0]);
if (!Number.isInteger(pinnedMajor) || pinnedMajor <= 0) {
  console.error(`error: invalid Node major version parsed from .nvmrc: ${JSON.stringify(pinnedNode)}`);
  process.exit(1);
}

const expectedEngines = [`>=${pinnedNode} <${pinnedMajor + 1}`, `>=${pinnedNode} <${pinnedMajor + 1}.0.0`];

// Ensure the repo's package.json "engines" stays in sync with the pinned .nvmrc
// so local tooling + CI agree on what's supported.
const rootPackageJsonPath = path.join(repoRoot, "package.json");
let rootPackageJson;
try {
  rootPackageJson = JSON.parse(fs.readFileSync(rootPackageJsonPath, "utf8"));
} catch (err) {
  console.error(`error: unable to read/parse ${rel(rootPackageJsonPath)}`);
  process.exit(1);
}

const rootEngines = rootPackageJson?.engines?.node;
if (typeof rootEngines !== "string" || rootEngines.trim() === "") {
  console.error(`error: ${rel(rootPackageJsonPath)} is missing engines.node`);
  process.exit(1);
}
if (!expectedEngines.includes(rootEngines.trim())) {
  console.error(`error: ${rel(rootPackageJsonPath)} engines.node is out of sync with .nvmrc`);
  console.error(`- .nvmrc: ${pinnedNode}`);
  console.error(`- engines.node: ${rootEngines}`);
  console.error(`- expected: one of ${expectedEngines.map((v) => JSON.stringify(v)).join(", ")}`);
  process.exit(1);
}

function getWorkspacePatterns(workspaces) {
  if (Array.isArray(workspaces)) return workspaces;
  if (workspaces && typeof workspaces === "object" && Array.isArray(workspaces.packages)) return workspaces.packages;
  return [];
}

function expandWorkspacePattern(pattern) {
  const normalized = pattern.replace(/\\/g, "/");
  if (!normalized.includes("*")) return [normalized];

  // Support the common "dir/*" form used by npm workspaces.
  if (normalized.endsWith("/*") && !normalized.slice(0, -2).includes("*")) {
    const base = normalized.slice(0, -2);
    const absBase = path.join(repoRoot, base);
    if (!fs.existsSync(absBase)) return [];
    return fs
      .readdirSync(absBase, { withFileTypes: true })
      .filter((entry) => entry.isDirectory())
      .map((entry) => path.join(base, entry.name).replace(/\\/g, "/"));
  }

  // Anything more complex is ignored (keep this check lightweight).
  return [];
}

// Ensure every workspace package.json declares the same engines.node string as the
// repo root (so CI-executed packages don't drift).
const workspacePatterns = getWorkspacePatterns(rootPackageJson.workspaces);
const workspaceDirs = workspacePatterns.flatMap(expandWorkspacePattern);
for (const dir of workspaceDirs) {
  const pkgPath = path.join(repoRoot, dir, "package.json");
  if (!fs.existsSync(pkgPath)) continue;
  const pkg = JSON.parse(fs.readFileSync(pkgPath, "utf8"));
  const engines = pkg?.engines?.node;
  if (engines !== rootEngines) {
    console.error(`error: engines.node mismatch in ${rel(pkgPath)}`);
    console.error(`- expected: ${rootEngines}`);
    console.error(`- actual:   ${engines ?? "(missing)"}`);
    process.exit(1);
  }
}

if (!fs.existsSync(workflowsDir)) {
  console.error(`error: workflows directory not found: ${rel(workflowsDir)}`);
  process.exit(1);
}

const workflowFiles = fs
  .readdirSync(workflowsDir, { withFileTypes: true })
  .filter((entry) => entry.isFile() && (entry.name.endsWith(".yml") || entry.name.endsWith(".yaml")))
  .map((entry) => entry.name)
  .sort();

/** @type {Array<{file: string, line: number, message: string}>} */
const errors = [];

for (const filename of workflowFiles) {
  const filePath = path.join(workflowsDir, filename);
  const contents = fs.readFileSync(filePath, "utf8");
  const lines = contents.split(/\r?\n/);

  for (let i = 0; i < lines.length; i += 1) {
    if (!/^\s*-\s*uses:\s*actions\/setup-node@v4\b/.test(lines[i])) continue;

    const indent = lines[i].match(/^(\s*)/)?.[1]?.length ?? 0;
    const stepLines = [];
    for (let j = i + 1; j < lines.length; j += 1) {
      // New step at the same indentation -> end of current step.
      if (new RegExp(`^\\s{${indent}}-\\s`).test(lines[j])) break;
      stepLines.push(lines[j]);
    }

    const hasNodeVersion = stepLines.some((line) => /^\s*node-version\s*:/.test(line));
    if (hasNodeVersion) {
      errors.push({
        file: filename,
        line: i + 1,
        message: "actions/setup-node@v4 uses 'node-version:'. Use 'node-version-file: .nvmrc' instead.",
      });
    }

    const nodeVersionFileLine = stepLines.find((line) => /^\s*node-version-file\s*:/.test(line));
    if (!nodeVersionFileLine) {
      errors.push({
        file: filename,
        line: i + 1,
        message: "actions/setup-node@v4 is missing 'node-version-file: .nvmrc'.",
      });
      continue;
    }

    const rawValue = nodeVersionFileLine.split(":").slice(1).join(":");
    const value = stripQuotes(rawValue);
    if (!value.endsWith(".nvmrc")) {
      errors.push({
        file: filename,
        line: i + 1,
        message: `node-version-file should point at a .nvmrc file (got ${JSON.stringify(value)}).`,
      });
    }
  }
}

if (errors.length) {
  console.error("error: Node workflow pinning violations detected.");
  console.error("");
  console.error("This repo standardizes Node.js via the root `.nvmrc`.");
  console.error("Any workflow step that uses `actions/setup-node@v4` must use:");
  console.error("  with:");
  console.error("    node-version-file: .nvmrc");
  console.error("");
  console.error("For workflows that checkout into a subdirectory (actions/checkout `path:`),");
  console.error("use the corresponding path (e.g. head/.nvmrc).");
  console.error("");
  console.error("Violations:");
  for (const err of errors) {
    console.error(`- .github/workflows/${err.file}:${err.line}: ${err.message}`);
  }
  process.exit(1);
}

console.log("Node workflow pinning: OK");
