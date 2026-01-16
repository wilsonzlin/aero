import fs from "node:fs";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { appendEnv, appendOutput, expandHome, fail, normalizeRel, resolveWorkspaceRoot } from "../_shared/github_io.mjs";
import { actionTimeoutMs } from "../_shared/exec.mjs";

// Use shared helpers to keep behavior aligned across actions.

function runDetectNodeDir(workspace) {
  const detectScript = path.resolve(workspace, "scripts/ci/detect-node-dir.mjs");
  if (!fs.existsSync(detectScript)) return null;

  const result = spawnSync(process.execPath, [detectScript], {
    cwd: workspace,
    stdio: ["ignore", "pipe", "pipe"],
    encoding: "utf8",
    timeout: actionTimeoutMs(30_000),
  });
  if ((result.status ?? 1) !== 0) {
    const details = (result.stderr || result.stdout || "").trim();
    fail(`setup-playwright: detect-node-dir failed.\n\n${details}`);
  }

  const out = new Map();
  for (const line of String(result.stdout || "").split(/\r?\n/u)) {
    const idx = line.indexOf("=");
    if (idx === -1) continue;
    const key = line.slice(0, idx).trim();
    const value = line.slice(idx + 1).trim();
    out.set(key, value);
  }
  return {
    dir: out.get("dir") || "",
    lockfile: out.get("lockfile") || "",
  };
}

function findNearestLockfile(workspaceAbs, startDirAbs) {
  const candidates = ["package-lock.json", "npm-shrinkwrap.json"];
  let cursor = startDirAbs;
  for (;;) {
    for (const name of candidates) {
      const candidate = path.join(cursor, name);
      if (fs.existsSync(candidate)) {
        return path.relative(workspaceAbs, candidate).replaceAll("\\", "/");
      }
    }
    if (path.resolve(cursor) === workspaceAbs) return "";
    const parent = path.dirname(cursor);
    if (parent === cursor) return "";
    const rel = path.relative(workspaceAbs, parent);
    if (rel.startsWith("..") || path.isAbsolute(rel)) return "";
    cursor = parent;
  }
}

const workspace = resolveWorkspaceRoot();

const browsersRaw = (process.env.INPUT_BROWSERS || "").trim() || (process.env.INPUT_PROJECT || "").trim() || "chromium";
const browsers = browsersRaw.split(/\s+/u).filter(Boolean);
const browsersKey = [...new Set(browsers)].sort().join("-");

let workingDirectory = (process.env.INPUT_WORKING_DIRECTORY || "").trim();
let resolvedLockfile = "";
if (!workingDirectory) {
  const detected = runDetectNodeDir(workspace);
  if (detected) {
    workingDirectory = detected.dir;
    resolvedLockfile = detected.lockfile;
  } else {
    const aeroNodeDir = (process.env.AERO_NODE_DIR || "").trim();
    const candidates = [aeroNodeDir || null, ".", "frontend", "web"].filter(Boolean);
    for (const dir of candidates) {
      const pkg = path.resolve(workspace, dir, "package.json");
      if (fs.existsSync(pkg)) {
        workingDirectory = dir;
        break;
      }
    }
  }
}

if (!workingDirectory) {
  fail(
    "setup-playwright: could not resolve a Node workspace. " +
      "Set the `working-directory` input (or AERO_NODE_DIR) to the directory containing package.json.",
  );
}

workingDirectory = normalizeRel(workingDirectory);
const workingDirAbs = path.resolve(workspace, workingDirectory);

let lockfile = (process.env.INPUT_LOCKFILE || "").trim();
if (lockfile) {
  if (path.isAbsolute(lockfile)) lockfile = path.relative(workspace, lockfile);
  lockfile = lockfile.replaceAll("\\", "/");
} else if (resolvedLockfile) {
  lockfile = resolvedLockfile;
  if (path.isAbsolute(lockfile)) lockfile = path.relative(workspace, lockfile);
  lockfile = lockfile.replaceAll("\\", "/");
} else {
  lockfile = findNearestLockfile(path.resolve(workspace), workingDirAbs);
}

let cacheKeyFile = lockfile;
if (!cacheKeyFile) {
  const pkg = path.join(workingDirAbs, "package.json");
  if (!fs.existsSync(pkg)) {
    fail(`setup-playwright: expected package.json at "${path.relative(workspace, pkg)}" but it does not exist.`);
  }
  cacheKeyFile = path.relative(workspace, pkg).replaceAll("\\", "/");
} else {
  const abs = path.resolve(workspace, cacheKeyFile);
  if (!fs.existsSync(abs)) {
    fail(`setup-playwright: lockfile "${cacheKeyFile}" does not exist.`);
  }
}

const cachePathInput = (process.env.INPUT_CACHE_PATH || "~/.cache/ms-playwright").trim();
let cachePath = expandHome(cachePathInput);
if (!path.isAbsolute(cachePath)) cachePath = path.resolve(workspace, cachePath);

const runnerOs = process.env.RUNNER_OS || "";
const withDepsInput = (process.env.INPUT_WITH_DEPS || "").trim().toLowerCase();
const withDeps = runnerOs === "Linux" ? (withDepsInput ? withDepsInput === "true" : true) : false;

const cachePathForGitHub = cachePath.replaceAll("\\", "/");

appendEnv("PLAYWRIGHT_BROWSERS_PATH", cachePathForGitHub);
appendEnv("PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD", "1");

appendOutput("browsers", browsers.join(" "));
appendOutput("browsers_key", browsersKey || "none");
appendOutput("working_directory", workingDirectory.replaceAll("\\", "/"));
appendOutput("lockfile", lockfile);
appendOutput("cache_key_file", cacheKeyFile);
appendOutput("cache_path", cachePathForGitHub);
appendOutput("with_deps", withDeps ? "true" : "false");
