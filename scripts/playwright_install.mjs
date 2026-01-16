#!/usr/bin/env node
import { execFileSync } from "node:child_process";
import process from "node:process";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);

function truthyEnv(value) {
  if (!value) return false;
  const v = String(value).toLowerCase();
  return v === "1" || v === "true";
}

function parseArgs(argv) {
  const options = {
    browser: "chromium",
    withDeps: false,
    help: false,
  };

  const args = [...argv];
  while (args.length) {
    const arg = args.shift();
    if (!arg) continue;
    if (arg === "--help" || arg === "-h") {
      options.help = true;
      continue;
    }
    if (arg === "--with-deps") {
      options.withDeps = true;
      continue;
    }
    if (arg.startsWith("--")) {
      throw new Error(`Unknown flag: ${arg}`);
    }
    options.browser = arg;
  }

  return options;
}

function main() {
  const opts = parseArgs(process.argv.slice(2));
  if (opts.help) {
    process.stdout.write(`Usage: node scripts/playwright_install.mjs [browser] [--with-deps]

Installs Playwright browsers in a shell-free way.

Environment variables respected:
  - PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1|true: skip install (exit 0)
  - PLAYWRIGHT_WITH_DEPS=1|true: pass --with-deps on Linux
  - GITHUB_ACTIONS=true: skip install (exit 0)

Examples:
  node scripts/playwright_install.mjs chromium
  node scripts/playwright_install.mjs chromium --with-deps
`);
    return;
  }

  const skip = truthyEnv(process.env.PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD) || process.env.GITHUB_ACTIONS === "true";
  if (skip) return;

  const args = ["install"];
  if (process.platform === "linux" && (opts.withDeps || truthyEnv(process.env.PLAYWRIGHT_WITH_DEPS))) {
    args.push("--with-deps");
  }
  args.push(opts.browser);

  const cli = require.resolve("@playwright/test/cli");
  // eslint-disable-next-line no-console
  console.log("Running:", process.execPath, cli, ...args);
  execFileSync(process.execPath, [cli, ...args], { stdio: "inherit" });
}

try {
  main();
} catch (err) {
  // eslint-disable-next-line no-console
  console.error(err?.stack || err);
  process.exitCode = 1;
}

