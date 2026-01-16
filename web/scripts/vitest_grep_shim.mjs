import { spawn } from "node:child_process";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);

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
  let runInBand = false;
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    if (arg === "--runInBand") {
      runInBand = true;
      continue;
    }
    if (arg.startsWith("--runInBand=")) {
      const raw = arg.slice("--runInBand=".length).toLowerCase();
      runInBand = raw !== "false" && raw !== "0";
      continue;
    }
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

  if (runInBand && !out.some((v) => v.startsWith("--poolOptions.threads.singleThread"))) {
    // Jest uses `--runInBand` to force serial execution. Vitest uses the thread pool option.
    out.push("--poolOptions.threads.singleThread");
  }

  return { args: out, patterns };
}

const { args, patterns } = translateArgs(process.argv.slice(2));

if (patterns.length) {
  // Vitest expects a regexp source string here (same idea as Mocha/Jest).
  // If multiple `--grep` flags are provided, match any of them.
  args.push("--testNamePattern", patterns.length === 1 ? patterns[0] : patterns.map((p) => `(?:${p})`).join("|"));
}

function resolveVitestCli() {
  // Avoid `shell: true` on Windows by invoking Vitest through Node directly.
  // This prevents cmd.exe metacharacter injection when patterns contain `&`, `|`, etc.
  const candidates = [
    "vitest/vitest.mjs",
    "vitest/node.mjs",
    "vitest/dist/cli.js",
  ];
  for (const id of candidates) {
    try {
      return require.resolve(id);
    } catch {
      // try next candidate
    }
  }
  throw new Error("Failed to resolve Vitest CLI entrypoint");
}

const vitestCli = resolveVitestCli();

const child = spawn(process.execPath, [vitestCli, "run", ...args], {
  stdio: "inherit",
});
child.on("exit", (code, signal) => {
  if (signal) process.kill(process.pid, signal);
  process.exit(code ?? 1);
});
