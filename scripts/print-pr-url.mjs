// SPDX-License-Identifier: MIT OR Apache-2.0

import { execFileSync } from "node:child_process";
import process from "node:process";

function git(args) {
  return execFileSync("git", args, { encoding: "utf8" }).trim();
}

function usage() {
  return `Usage: node scripts/print-pr-url.mjs [--actions]

Prints a GitHub compare URL for the current branch (suitable for opening a PR in the web UI).

Options:
  --actions   Also print the GitHub Actions URL for the same branch.
  --base <ref>    Base branch/ref (default: main)
  --remote <name> Git remote name (default: origin)
  --branch <ref>  Branch/ref to use instead of the current branch

Environment overrides:
  AERO_PR_BASE=<ref>        Base branch/ref (default: main)
  AERO_PR_REMOTE=<name>     Git remote name (default: origin)
  AERO_PR_BRANCH=<ref>      Branch/ref to use instead of the current branch
  AERO_PR_INCLUDE_ACTIONS_URL=1  (legacy) same as --actions
`;
}

function parseGitHubOrigin(url) {
  const u = url.trim();

  // git@github.com:owner/repo(.git)
  let m = u.match(/^git@github\.com:([^/]+)\/(.+?)(?:\.git)?$/);
  if (m) return { owner: m[1], repo: m[2] };

  // https://github.com/owner/repo(.git)
  m = u.match(/^https?:\/\/github\.com\/([^/]+)\/(.+?)(?:\.git)?\/?$/);
  if (m) return { owner: m[1], repo: m[2] };

  // ssh://git@github.com/owner/repo(.git)
  m = u.match(/^ssh:\/\/git@github\.com\/([^/]+)\/(.+?)(?:\.git)?\/?$/);
  if (m) return { owner: m[1], repo: m[2] };

  return null;
}

function encodeCompareRef(ref) {
  // Keep path readability (branch names commonly contain "/"), while still encoding any
  // characters that could break the URL.
  return encodeURIComponent(ref).replaceAll("%2F", "/");
}

function truthyEnv(name) {
  const raw = (process.env[name] || "").trim();
  if (!raw) return false;
  return !["0", "false", "FALSE", "no", "NO", "off", "OFF"].includes(raw);
}

function parseArgValue(arg) {
  const eq = arg.indexOf("=");
  if (eq === -1) return null;
  const key = arg.slice(0, eq);
  const value = arg.slice(eq + 1);
  return { key, value };
}

function main() {
  const args = process.argv.slice(2);
  let includeActionsUrl = truthyEnv("AERO_PR_INCLUDE_ACTIONS_URL");

  let base = (process.env.AERO_PR_BASE || "main").trim() || "main";
  let remote = (process.env.AERO_PR_REMOTE || "origin").trim() || "origin";
  let branchOverride = (process.env.AERO_PR_BRANCH || "").trim();

  let i = 0;
  while (i < args.length) {
    const arg = args[i];
    const kv = parseArgValue(arg);

    if (kv && kv.key === "--base") {
      base = kv.value.trim() || base;
      i += 1;
      continue;
    }
    if (kv && kv.key === "--remote") {
      remote = kv.value.trim() || remote;
      i += 1;
      continue;
    }
    if (kv && kv.key === "--branch") {
      branchOverride = kv.value.trim() || branchOverride;
      i += 1;
      continue;
    }

    if (arg === "--base" || arg === "--remote" || arg === "--branch") {
      const next = args[i + 1];
      if (typeof next !== "string" || next.startsWith("-")) {
        console.error(`error: ${arg} requires a value`);
        console.error("");
        process.stderr.write(usage());
        process.exitCode = 1;
        return;
      }
      if (arg === "--base") base = next.trim() || base;
      if (arg === "--remote") remote = next.trim() || remote;
      if (arg === "--branch") branchOverride = next.trim() || branchOverride;
      i += 2;
      continue;
    }

    if (arg === "--actions" || arg === "--include-actions-url") {
      includeActionsUrl = true;
      i += 1;
      continue;
    }
    if (arg === "--help" || arg === "-h") {
      process.stdout.write(usage());
      return;
    }

    console.error(`error: unknown argument: ${arg}`);
    console.error("");
    process.stderr.write(usage());
    process.exitCode = 1;
    return;
  }

  let branch = "";
  try {
    branch = git(["branch", "--show-current"]);
  } catch (err) {
    console.error("error: failed to read current git branch");
    console.error(String(err && err.message ? err.message : err));
    process.exitCode = 1;
    return;
  }

  if (!branch && !branchOverride) {
    console.error("error: not on a branch (detached HEAD)");
    console.error("tip: checkout a branch, or pass a branch ref explicitly with --branch <ref> (or AERO_PR_BRANCH=<ref>).");
    process.exitCode = 1;
    return;
  }

  const compareBranch = branchOverride || branch;

  let originUrl = "";
  try {
    originUrl = git(["remote", "get-url", remote]);
  } catch (err) {
    console.error(`error: failed to read git remote URL for '${remote}'`);
    console.error(String(err && err.message ? err.message : err));
    process.exitCode = 1;
    return;
  }

  const gh = parseGitHubOrigin(originUrl);
  if (!gh) {
    console.error("error: remote does not look like a GitHub repo URL");
    console.error(`- remote '${remote}': ${originUrl}`);
    console.error("tip: open a PR manually, or set AERO_PR_REMOTE to a GitHub remote.");
    process.exitCode = 1;
    return;
  }

  const compareUrl = `https://github.com/${gh.owner}/${gh.repo}/compare/${encodeCompareRef(base)}...${encodeCompareRef(compareBranch)}?expand=1`;
  process.stdout.write(`${compareUrl}\n`);

  if (includeActionsUrl) {
    const actionsUrl = `https://github.com/${gh.owner}/${gh.repo}/actions?query=branch%3A${encodeCompareRef(compareBranch)}`;
    process.stdout.write(`${actionsUrl}\n`);
  }
}

main();

