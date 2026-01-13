import { spawn } from "node:child_process";

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
  const patterns = [];
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    if (arg === "--grep" || arg === "-g") {
      const pattern = argv[i + 1];
      if (pattern) {
        patterns.push(pattern);
        i++;
      }
      continue;
    }
    if (arg.startsWith("--grep=")) {
      const pattern = arg.slice("--grep=".length);
      if (pattern) patterns.push(pattern);
      continue;
    }
    out.push(arg);
  }
  return { args: out, patterns };
}

const { args, patterns } = translateArgs(process.argv.slice(2));

if (patterns.length) {
  // Vitest expects a regexp source string here (same idea as Mocha/Jest).
  // If multiple `--grep` flags are provided, match any of them.
  args.push("--testNamePattern", patterns.length === 1 ? patterns[0] : patterns.map((p) => `(?:${p})`).join("|"));
}

const child = spawn("vitest", ["run", ...args], {
  stdio: "inherit",
  // `shell: true` is required for Windows because `vitest` is a `.cmd` shim.
  shell: process.platform === "win32",
});
child.on("exit", (code, signal) => {
  if (signal) process.kill(process.pid, signal);
  process.exit(code ?? 1);
});
