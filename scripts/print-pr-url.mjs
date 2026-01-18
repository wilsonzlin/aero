import { execFileSync } from "node:child_process";
import process from "node:process";

function git(args) {
  return execFileSync("git", args, { encoding: "utf8" }).trim();
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

function main() {
  const base = (process.env.AERO_PR_BASE || "main").trim() || "main";
  const remote = (process.env.AERO_PR_REMOTE || "origin").trim() || "origin";
  const includeActionsUrl = truthyEnv("AERO_PR_INCLUDE_ACTIONS_URL");

  let branch = "";
  try {
    branch = git(["branch", "--show-current"]);
  } catch (err) {
    console.error("error: failed to read current git branch");
    console.error(String(err && err.message ? err.message : err));
    process.exitCode = 1;
    return;
  }

  if (!branch) {
    console.error("error: not on a branch (detached HEAD)");
    console.error("tip: checkout a branch, or pass a branch ref explicitly by setting AERO_PR_BRANCH=<ref>.");
    process.exitCode = 1;
    return;
  }

  const branchOverride = (process.env.AERO_PR_BRANCH || "").trim();
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

