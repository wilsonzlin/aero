#!/usr/bin/env node
import { execSync } from "node:child_process";
import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(scriptDir, "..", "..");

function run(cmd) {
  execSync(cmd, { cwd: repoRoot, stdio: "inherit" });
}

function read(cmd) {
  return execSync(cmd, { cwd: repoRoot, encoding: "utf8" });
}

try {
  run("npm run generate:goldens");
} catch (err) {
  console.error("\nerror: failed to run golden generator (`npm run generate:goldens`).");
  process.exit(1);
}

try {
  // Match CI usage (no `--` separator) so local runs behave the same way.
  run("git diff --exit-code tests/golden");
} catch {
  console.error(
    "\nGenerated golden images are out of date.\n" +
      "Run `npm run generate:goldens` and commit the updated PNGs under `tests/golden/`.\n",
  );
  process.exit(1);
}

const status = read("git status --porcelain -- tests/golden").trim();
if (status !== "") {
  console.error(
    "\nGenerated golden images are out of date (untracked or unstaged changes detected).\n" +
      "Run `npm run generate:goldens` and commit the updated PNGs under `tests/golden/`.\n" +
      "\nChanged files:\n" +
      status +
      "\n",
  );
  process.exit(1);
}

console.log("Golden images: OK");

