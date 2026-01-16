#!/usr/bin/env node
import { execFileSync } from "node:child_process";
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

function stripYamlComment(value) {
  let inSingle = false;
  let inDouble = false;
  for (let i = 0; i < value.length; i += 1) {
    const ch = value[i];
    if (ch === "'" && !inDouble) {
      inSingle = !inSingle;
      continue;
    }
    if (ch === '"' && !inSingle) {
      inDouble = !inDouble;
      continue;
    }
    if (ch === "#" && !inSingle && !inDouble) {
      // YAML comments start with `#` when it appears at the start of the value or is
      // preceded by whitespace. Keep this lightweight so we can scan workflows without a
      // full YAML parser.
      if (i === 0 || /\s/.test(value[i - 1])) {
        return value.slice(0, i).trimEnd();
      }
    }
  }
  return value;
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

function listRepoFiles() {
  try {
    const out = execFileSync("git", ["ls-files"], { cwd: repoRoot, encoding: "utf8" });
    return out.split(/\r?\n/).filter(Boolean);
  } catch {
    // Best-effort fallback for environments that do not have git metadata.
    return [];
  }
}

function listDockerfiles() {
  const files = listRepoFiles();
  const dockerfiles = files.filter((file) => path.basename(file) === "Dockerfile");
  if (dockerfiles.length) return dockerfiles;

  // Keep the legacy explicit list so local runs outside git still validate the
  // Dockerfiles that currently pin Node.
  return ["backend/aero-gateway/Dockerfile", "server/Dockerfile", "tools/net-proxy-server/Dockerfile"].filter((file) =>
    fs.existsSync(path.join(repoRoot, file)),
  );
}

function checkDockerfilePinnedNode(fileRel) {
  const filePath = path.join(repoRoot, fileRel);
  if (!fs.existsSync(filePath)) return;

  const lines = fs.readFileSync(filePath, "utf8").split(/\r?\n/);
  /** @type {{line: number, version: string} | null} */
  let nodeVersionArg = null;
  for (let i = 0; i < lines.length; i += 1) {
    const match = lines[i].match(/^\s*ARG\s+NODE_VERSION\s*=\s*(\S+)\s*(?:#.*)?$/);
    if (!match) continue;

    const version = match[1];
    if (!/^\d+\.\d+\.\d+$/.test(version)) {
      console.error(`error: Dockerfile NODE_VERSION arg must be an exact semver version`);
      console.error(`- file: ${rel(filePath)}:${i + 1}`);
      console.error(`- found: ${version}`);
      console.error(`- expected: ${pinnedNode}`);
      process.exit(1);
    }
    if (version !== pinnedNode) {
      console.error(`error: Dockerfile NODE_VERSION arg is out of sync with .nvmrc`);
      console.error(`- file: ${rel(filePath)}:${i + 1}`);
      console.error(`- found: ${version}`);
      console.error(`- expected: ${pinnedNode}`);
      process.exit(1);
    }
    if (!nodeVersionArg) nodeVersionArg = { line: i, version };
  }

  for (let i = 0; i < lines.length; i += 1) {
    const match = lines[i].match(/^\s*FROM\s+(?:--platform=\S+\s+)?node:([^\s]+)\b/);
    if (!match) continue;

    const tag = match[1];
    const versionMatch = tag.match(/^(\d+\.\d+\.\d+)\b/);
    const version = versionMatch ? versionMatch[1] : null;
    if (version) {
      if (version !== pinnedNode) {
        console.error(`error: Dockerfile Node version is out of sync with .nvmrc`);
        console.error(`- file: ${rel(filePath)}:${i + 1}`);
        console.error(`- found: ${version}`);
        console.error(`- expected: ${pinnedNode}`);
        process.exit(1);
      }
      continue;
    }

    const usesNodeVersionArg = tag.startsWith("${NODE_VERSION}") || tag.startsWith("$NODE_VERSION");
    if (usesNodeVersionArg) {
      if (!nodeVersionArg) {
        console.error(`error: Dockerfile uses node:${tag} but is missing a default ARG NODE_VERSION=<pinned> declaration`);
        console.error(`- file: ${rel(filePath)}:${i + 1}`);
        console.error(`- expected: ARG NODE_VERSION=${pinnedNode}`);
        process.exit(1);
      }
      if (nodeVersionArg.line > i) {
        console.error(`error: Dockerfile ARG NODE_VERSION must be declared before it is used in FROM`);
        console.error(`- file: ${rel(filePath)}:${i + 1}`);
        console.error(`- arg declared at: ${rel(filePath)}:${nodeVersionArg.line + 1}`);
        process.exit(1);
      }
      continue;
    }

    console.error(`error: Dockerfile Node base image must use an exact semver tag`);
    console.error(`- file: ${rel(filePath)}:${i + 1}`);
    console.error(`- found: node:${tag}`);
    console.error(`- expected: node:${pinnedNode}-<variant> (or similar), or node:\${NODE_VERSION}-<variant> with ARG NODE_VERSION=${pinnedNode}`);
    process.exit(1);
  }
}

for (const dockerfile of listDockerfiles()) {
  checkDockerfilePinnedNode(dockerfile);
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
    // Most workflows format steps as:
    //   - name: Setup Node
    //     uses: actions/setup-node@v4
    // but some omit the name and use:
    //   - uses: actions/setup-node@v4
    //
    // We intentionally enforce pinning even for the common multi-line form.
    if (!/^\s*(?:-\s*)?uses:\s*actions\/setup-node@v4\b/.test(lines[i])) continue;

    const usesLine = i;
    const usesIndent = lines[i].match(/^(\s*)/)?.[1]?.length ?? 0;

    let stepIndent = usesIndent;
    let stepStart = i;
    if (/^\s*uses:\s*actions\/setup-node@v4\b/.test(lines[i])) {
      // `uses:` is indented under a preceding `- name:` list item.
      stepIndent = Math.max(0, usesIndent - 2);
      for (let k = i - 1; k >= 0; k -= 1) {
        const line = lines[k];
        const trimmed = line.trim();
        if (trimmed === "" || trimmed.startsWith("#")) continue;
        const indent = line.match(/^(\s*)/)?.[1]?.length ?? 0;
        if (indent < stepIndent) break;
        if (new RegExp(`^\\s{${stepIndent}}-\\s`).test(line)) {
          stepStart = k;
          break;
        }
      }
    }

    let stepEnd = lines.length;
    for (let j = stepStart + 1; j < lines.length; j += 1) {
      // New step at the same indentation -> end of current step.
      if (new RegExp(`^\\s{${stepIndent}}-\\s`).test(lines[j])) {
        stepEnd = j;
        break;
      }
    }

    const stepLines = lines.slice(stepStart, stepEnd);

    const hasNodeVersion = stepLines.some((line) => /^\s*node-version\s*:/.test(line));
    if (hasNodeVersion) {
      errors.push({
        file: filename,
        line: usesLine + 1,
        message: "actions/setup-node@v4 uses 'node-version:'. Use 'node-version-file: .nvmrc' instead.",
      });
    }

    const nodeVersionFileLine = stepLines.find((line) => /^\s*node-version-file\s*:/.test(line));
    if (!nodeVersionFileLine) {
      errors.push({
        file: filename,
        line: usesLine + 1,
        message: "actions/setup-node@v4 is missing 'node-version-file: .nvmrc'.",
      });
      i = stepEnd - 1;
      continue;
    }

    const rawValue = nodeVersionFileLine.split(":").slice(1).join(":");
    const value = stripQuotes(stripYamlComment(rawValue));
    if (!value.endsWith(".nvmrc")) {
      errors.push({
        file: filename,
        line: usesLine + 1,
        message: `node-version-file should point at a .nvmrc file (got ${JSON.stringify(value)}).`,
      });
    }

    // Don't re-scan the interior of the current step.
    i = stepEnd - 1;
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
