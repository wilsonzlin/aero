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
