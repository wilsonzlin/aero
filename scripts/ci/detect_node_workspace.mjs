#!/usr/bin/env node
import fs from "node:fs";
import path from "node:path";
import process from "node:process";

function usage(exitCode) {
  const msg = `
Usage:
  node scripts/ci/detect_node_workspace.mjs [--root <dir>] [--json]

Detects the Node workspace directory (where package.json lives) within a checkout.

Resolution order:
  1) $AERO_NODE_DIR (relative to --root)
  2) <root>/package.json
  3) <root>/frontend/package.json
  4) <root>/web/package.json

Outputs (default): key=value lines suitable for $GITHUB_OUTPUT
  - dir=<workspace dir>
  - lockfile=<path to package-lock.json>

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

function nonEmpty(value) {
  if (typeof value !== "string") return null;
  const v = value.trim();
  return v.length ? v : null;
}

function main() {
  const opts = parseArgs(process.argv.slice(2));

  const rootArg = opts.root ?? ".";
  const rootAbs = path.resolve(process.cwd(), rootArg);

  const override = nonEmpty(process.env.AERO_NODE_DIR);
  const candidates = override ? [override] : [".", "frontend", "web"];

  let workspaceRel = null;
  for (const rel of candidates) {
    const pkg = rel === "." ? path.join(rootAbs, "package.json") : path.join(rootAbs, rel, "package.json");
    if (fs.existsSync(pkg)) {
      workspaceRel = rel;
      break;
    }
  }

  if (!workspaceRel) {
    // eslint-disable-next-line no-console
    console.error(
      `package.json not found under ${rootArg}. Set AERO_NODE_DIR to the directory containing package.json.`,
    );
    process.exit(1);
  }

  const lockfileAbs =
    workspaceRel === "."
      ? path.join(rootAbs, "package-lock.json")
      : path.join(rootAbs, workspaceRel, "package-lock.json");

  if (!fs.existsSync(lockfileAbs)) {
    // eslint-disable-next-line no-console
    console.error(
      `package-lock.json not found at '${path.relative(process.cwd(), lockfileAbs)}'. This workflow expects npm; ensure a lockfile exists.`,
    );
    process.exit(1);
  }

  const dirOut = workspaceRel === "." ? rootArg : path.join(rootArg, workspaceRel);
  const lockfileOut =
    workspaceRel === "." ? path.join(rootArg, "package-lock.json") : path.join(rootArg, workspaceRel, "package-lock.json");

  if (opts.json) {
    // eslint-disable-next-line no-console
    console.log(JSON.stringify({ dir: dirOut, lockfile: lockfileOut }, null, 2));
    return;
  }

  // key=value pairs: suitable for `>> $GITHUB_OUTPUT`
  // eslint-disable-next-line no-console
  console.log(`dir=${dirOut}`);
  // eslint-disable-next-line no-console
  console.log(`lockfile=${lockfileOut}`);
}

main();

