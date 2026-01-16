#!/usr/bin/env node
import { execFileSync } from "node:child_process";
import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(scriptDir, "..", "..");
const isGitHubActions = process.env.GITHUB_ACTIONS === "true";

function run(cmd, args) {
  execFileSync(cmd, args, { cwd: repoRoot, stdio: "inherit" });
}

function read(cmd, args) {
  return execFileSync(cmd, args, { cwd: repoRoot, encoding: "utf8" });
}

function fail(message) {
  if (isGitHubActions) {
    // Attach a clear top-level annotation. Also print the full message to stderr.
    console.error(`::error::${message}`);
  }
  console.error(message);
  process.exit(1);
}

try {
  run("npm", ["run", "check:node"]);
} catch {
  fail("error: failed to run Node version check (`npm run check:node`).");
}

try {
  run("npm", ["run", "generate:goldens"]);
} catch {
  fail("error: failed to run golden generator (`npm run generate:goldens`).");
}

try {
  // Match CI usage (no `--` separator) so local runs behave the same way.
  run("git", ["diff", "--exit-code", "tests/golden"]);
} catch {
  const status = read("git", ["status", "--porcelain", "--", "tests/golden"]).trim();
  const detail = status ? `\nChanged files:\n${status}\n` : "";
  fail(
    `Generated golden images are out of date.\n` +
      `Run \`npm run generate:goldens\` and commit the updated PNGs under \`tests/golden/\`.\n` +
      detail,
  );
}

const status = read("git", ["status", "--porcelain", "--", "tests/golden"]).trim();
if (status !== "") {
  fail(
    `Generated golden images are out of date (untracked or unstaged changes detected).\n` +
      `Run \`npm run generate:goldens\` and commit the updated PNGs under \`tests/golden/\`.\n\n` +
      `Changed files:\n${status}\n`,
  );
}

console.log("Golden images: OK");
