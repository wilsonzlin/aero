#!/usr/bin/env node
/**
 * Deprecated wrapper around `detect-node-dir.mjs`.
 *
 * This script exists for backwards compatibility with older CI workflows that
 * referenced `detect_node_workspace.mjs`. New callers should use:
 *
 *   node scripts/ci/detect-node-dir.mjs [--root <dir>] --require-lockfile
 */

import { spawnSync } from "node:child_process";
import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

function usage(exitCode) {
  const msg = `
Usage:
  node scripts/ci/detect_node_workspace.mjs [--root <dir>] [--json]

Detects the Node workspace directory (where package.json lives) within a checkout.

This is a compatibility wrapper around scripts/ci/detect-node-dir.mjs.

Options:
  --root <dir>   Checkout root directory to search (default: .)
  --json         Print JSON instead of key=value pairs
  --help         Show this help
`.trim();
  // eslint-disable-next-line no-console
  console.log(msg);
  process.exit(exitCode);
}

function parseArgs(argv) {
  const opts = { root: ".", json: false };

  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    switch (arg) {
      case "--root":
        opts.root = argv[++i];
        break;
      case "--json":
        opts.json = true;
        break;
      case "--help":
        usage(0);
        break;
      default:
        if (arg.startsWith("-")) {
          // eslint-disable-next-line no-console
          console.error(`Unknown option: ${arg}`);
          usage(1);
        }
        break;
    }
  }

  return opts;
}

function parseKeyValue(stdout) {
  const out = {};
  for (const line of stdout.split(/\r?\n/u)) {
    const idx = line.indexOf("=");
    if (idx === -1) continue;
    const key = line.slice(0, idx).trim();
    const value = line.slice(idx + 1).trim();
    if (!key) continue;
    out[key] = value;
  }
  return out;
}

function main() {
  const opts = parseArgs(process.argv.slice(2));

  const __filename = fileURLToPath(import.meta.url);
  const __dirname = path.dirname(__filename);
  const detectScript = path.join(__dirname, "detect-node-dir.mjs");

  const args = ["--root", opts.root, "--require-lockfile"];

  // Forward stderr so the underlying resolver can log resolution details.
  const result = spawnSync(process.execPath, [detectScript, ...args], {
    cwd: process.cwd(),
    stdio: ["ignore", "pipe", "inherit"],
    encoding: "utf8",
  });

  if ((result.status ?? 1) !== 0) {
    process.exit(result.status ?? 1);
  }

  const stdout = result.stdout ?? "";
  if (opts.json) {
    // eslint-disable-next-line no-console
    console.log(JSON.stringify(parseKeyValue(stdout), null, 2));
    return;
  }

  process.stdout.write(stdout);
}

main();

