import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";

/**
 * Vitest does not currently support a Mocha-style `--grep` flag, but a large part of the repo's
 * developer docs use it for quick targeted runs:
 *
 *   npm test -- --grep <pattern>
 *
 * Provide a tiny compatibility shim by translating `--grep` into Vitest's `--testNamePattern` /
 * `-t` option.
 */
function translateArgs(argv) {
  const out = [];
  const filters = [];
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    if (arg === "--grep") {
      const pattern = argv[i + 1];
      if (pattern) {
        filters.push(pattern);
        i++;
      }
      continue;
    }
    if (arg.startsWith("--grep=")) {
      const pattern = arg.slice("--grep=".length);
      if (pattern) filters.push(pattern);
      continue;
    }
    out.push(arg);
  }
  return { args: out, filters };
}

const { args, filters } = translateArgs(process.argv.slice(2));
const vitestBin = fileURLToPath(new URL("../../node_modules/.bin/vitest", import.meta.url));

const child = spawn(process.execPath, [vitestBin, "run", ...args, ...filters], { stdio: "inherit" });
child.on("exit", (code, signal) => {
  if (signal) process.kill(process.pid, signal);
  process.exit(code ?? 1);
});
