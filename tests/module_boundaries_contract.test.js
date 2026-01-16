import assert from "node:assert/strict";
import test from "node:test";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { readdir, readFile, stat } from "node:fs/promises";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const repoRoot = path.resolve(__dirname, "..");

function escapeRegex(s) {
  return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

function buildWorkspaceSrcImportRegex(workspaceRel, extensionsAlternation) {
  const parts = workspaceRel.split("/").map(escapeRegex);
  const sep = String.raw`[\\/]+`;
  const workspace = parts.join(sep);
  return new RegExp(`${sep}${workspace}${sep}src${sep}[^"'\\s)]+\\.(?:${extensionsAlternation})\\b`, "g");
}

function matchToRepoFsPath(match) {
  let rel = match.replace(/^[\\/]+/, "");
  rel = rel.replace(/[\\/]+/g, "/");
  return path.join(repoRoot, ...rel.split("/"));
}

async function fileExists(filePath) {
  try {
    await stat(filePath);
    return true;
  } catch {
    return false;
  }
}

async function collectTestFiles(dir) {
  const out = [];
  const entries = await readdir(dir, { withFileTypes: true });
  for (const entry of entries) {
    const fullPath = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      out.push(...(await collectTestFiles(fullPath)));
      continue;
    }
    if (!entry.isFile()) continue;
    if (!entry.name.endsWith(".test.js") && !entry.name.endsWith(".test.ts")) continue;
    out.push(fullPath);
  }
  return out;
}

async function tryReadJsonFile(filePath) {
  try {
    const raw = await readFile(filePath, "utf8");
    return JSON.parse(raw);
  } catch {
    return null;
  }
}

async function expandWorkspaces(workspaces) {
  const expanded = [];
  for (const w of workspaces) {
    if (!w.includes("*")) {
      expanded.push(w);
      continue;
    }

    const base = w.slice(0, w.indexOf("*")).replace(/\/$/, "");
    const baseDir = path.join(repoRoot, base);
    let entries = [];
    try {
      entries = await readdir(baseDir, { withFileTypes: true });
    } catch {
      continue;
    }
    for (const entry of entries) {
      if (!entry.isDirectory()) continue;
      const rel = path.posix.join(base, entry.name);
      const pkgJsonPath = path.join(repoRoot, rel, "package.json");
      if ((await tryReadJsonFile(pkgJsonPath)) === null) continue;
      expanded.push(rel);
    }
  }
  return expanded.sort();
}

async function hasTypeScriptUnderSrc(rel) {
  // Efficient enough for our workspaces: walk `src/` recursively with early exit.
  const root = path.join(repoRoot, rel, "src");
  const stack = [root];
  while (stack.length) {
    const dir = stack.pop();
    if (!dir) break;
    let entries = [];
    try {
      entries = await readdir(dir, { withFileTypes: true });
    } catch {
      continue;
    }
    for (const entry of entries) {
      const fullPath = path.join(dir, entry.name);
      if (entry.isDirectory()) {
        stack.push(fullPath);
        continue;
      }
      if (!entry.isFile()) continue;
      if (entry.name.endsWith(".ts") || entry.name.endsWith(".tsx") || entry.name.endsWith(".mts") || entry.name.endsWith(".cts")) {
        return true;
      }
    }
  }
  return false;
}

test("module boundaries: repo-root tests must not import TS sources from CJS workspaces", async () => {
  const roots = [
    "tests",
    "backend/aero-gateway/test",
    "bench",
    "net-proxy/src/test",
    "server/test",
    "services/image-gateway/test",
    "tools/net-proxy-server/test",
    "tools/perf/tests",
    "tools/range-harness/test",
    "packages/aero-stats/test",
    "web/test",
    "emulator/protocol/tests",
  ].map((p) => path.join(repoRoot, p));

  const all = [];
  for (const dir of roots) {
    try {
      all.push(...(await collectTestFiles(dir)));
    } catch {
      // Ignore missing directories (workspaces may be pruned in some environments).
    }
  }

  const files = all
    .filter((p) => path.basename(p) !== path.basename(__filename))
    .map((p) => path.relative(repoRoot, p))
    .sort();

  const rootPkg = await tryReadJsonFile(path.join(repoRoot, "package.json"));
  const workspaceGlobs = Array.isArray(rootPkg?.workspaces) ? rootPkg.workspaces : [];
  const workspaceDirs = await expandWorkspaces(workspaceGlobs);

  const forbiddenWorkspaces = [];
  for (const rel of workspaceDirs) {
    const pkg = await tryReadJsonFile(path.join(repoRoot, rel, "package.json"));
    if (!pkg) continue;
    const type = pkg.type ?? null;
    if (type === "module") continue;
    if (!(await hasTypeScriptUnderSrc(rel))) continue;
    forbiddenWorkspaces.push(rel);
  }

  const violations = [];
  for (const name of files) {
    const fullPath = path.join(repoRoot, name);
    const content = await readFile(fullPath, "utf8");

    for (const rel of forbiddenWorkspaces) {
      // Direct TS imports are always forbidden for these workspaces.
      const tsRe = buildWorkspaceSrcImportRegex(rel, "ts|tsx|mts|cts");
      for (;;) {
        const m = tsRe.exec(content);
        if (!m) break;
        violations.push({
          file: name,
          rule: `workspace-ts-src:${rel}`,
          reason: `workspace ${rel} is not type=module; repo-root tests must not import TS sources from its src/ tree`,
        });
      }

      // `.js` specifiers used to reach TS sources (NodeNext-style) are forbidden *only if the
      // referenced `.js` file does not exist* (i.e. it likely maps to a `.ts` file).
      const jsRe = buildWorkspaceSrcImportRegex(rel, "js");
      for (;;) {
        const m = jsRe.exec(content);
        if (!m) break;
        const spec = m[0];
        const fsPath = matchToRepoFsPath(spec);
        if (await fileExists(fsPath)) continue;
        violations.push({
          file: name,
          rule: `workspace-ts-src-js-spec:${rel}`,
          reason: `workspace ${rel} is not type=module; repo-root tests must not import TS sources via .js specifiers from its src/ tree`,
        });
      }
    }
  }

  assert.deepEqual(violations, [], `Module boundary violations: ${JSON.stringify(violations, null, 2)}`);
});

test("module boundaries: src import scan matches slash and backslash separators", () => {
  const tsRe = buildWorkspaceSrcImportRegex("backend/aero-gateway", "ts|tsx|mts|cts");
  assert.ok(tsRe.test("import x from '../backend/aero-gateway/src/index.ts';"));
  tsRe.lastIndex = 0;
  assert.ok(tsRe.test("import x from '..\\\\backend\\\\aero-gateway\\\\src\\\\index.ts';"));

  const jsRe = buildWorkspaceSrcImportRegex("net-proxy", "js");
  assert.ok(jsRe.test("const p = '../net-proxy/src/text.js';"));
  jsRe.lastIndex = 0;
  assert.ok(jsRe.test("const p = '..\\\\net-proxy\\\\src\\\\text.js';"));
});

