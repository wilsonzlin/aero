#!/usr/bin/env node
import { spawn } from "node:child_process";
import fs from "node:fs/promises";
import net from "node:net";
import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";
import { unrefBestEffort } from "../../src/unref_safe.js";

function usage(exitCode) {
  const msg = `
Usage:
  node scripts/ci/run_browser_perf.mjs [options]

Runs the Playwright-based browser perf suite (tools/perf/run.mjs) and writes artifacts.

Options:
  --workspace <dir>     Directory containing package.json (default: .)
  --url <url>           Benchmark an already-running URL
  --preview             Build + start a preview server (npm run build + npm run preview)
  --preview-port <n>    Preview server port (default: 4173)
  --perf-runner <path>  Path to tools/perf/run.mjs (default: <repo>/tools/perf/run.mjs)
  --trace               Capture an Aero trace via tools/perf/run.mjs (best-effort; opt-in)
  --trace-duration-ms <n>
                        Capture a trace for a fixed duration
  --include-aero-bench  Include app-provided microbench suite if available
  --out-dir <dir>       Output directory (required)
  --iterations <n>      Iterations per benchmark (default: $PERF_ITERATIONS or 3)
  --help                Show this help

Outputs (in --out-dir):
  - raw.json
  - summary.json
  - perf_export.json
  - trace.json
`.trim();
  // eslint-disable-next-line no-console
  console.log(msg);
  process.exit(exitCode);
}

function parseArgs(argv) {
  const opts = {
    workspace: ".",
    url: null,
    preview: false,
    previewPort: null,
    perfRunner: null,
    trace: null,
    traceDurationMs: null,
    includeAeroBench: null,
    outDir: null,
    iterations: null,
  };

  const requireValue = (flag, idx) => {
    const next = argv[idx + 1];
    // Treat `--flag --other-flag` as missing value (common typo), while still
    // allowing negative numbers for numeric flags.
    if (next === undefined || (next.startsWith("-") && !/^-\d/u.test(next))) {
      // eslint-disable-next-line no-console
      console.error(`${flag} requires a value`);
      usage(1);
    }
    return next;
  };

  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    switch (arg) {
      case "--workspace":
        opts.workspace = requireValue("--workspace", i);
        i += 1;
        break;
      case "--url":
        opts.url = requireValue("--url", i);
        i += 1;
        break;
      case "--preview":
        opts.preview = true;
        break;
      case "--preview-port":
        opts.previewPort = Number.parseInt(requireValue("--preview-port", i), 10);
        i += 1;
        break;
      case "--perf-runner":
        opts.perfRunner = requireValue("--perf-runner", i);
        i += 1;
        if (!opts.perfRunner) {
          // eslint-disable-next-line no-console
          console.error("--perf-runner requires a value");
          usage(1);
        }
        break;
      case "--trace":
        opts.trace = true;
        break;
      case "--trace-duration-ms":
        opts.trace = true;
        opts.traceDurationMs = Number.parseInt(requireValue("--trace-duration-ms", i), 10);
        i += 1;
        break;
      case "--include-aero-bench":
        opts.includeAeroBench = true;
        break;
      case "--out-dir":
        opts.outDir = requireValue("--out-dir", i);
        i += 1;
        break;
      case "--iterations":
        opts.iterations = Number.parseInt(requireValue("--iterations", i), 10);
        i += 1;
        break;
      case "--help":
        usage(0);
        break;
      default:
        if (arg.startsWith("-")) {
          // eslint-disable-next-line no-console
          console.error(`Unknown option: ${arg}`);
          usage(1);
        } else if (arg && String(arg).trim() !== "") {
          // eslint-disable-next-line no-console
          console.error(`Unexpected argument: ${arg}`);
          usage(1);
        }
        break;
    }
  }

  if (!opts.outDir) {
    // eslint-disable-next-line no-console
    console.error("--out-dir is required");
    usage(1);
  }

  if ((opts.url ? 1 : 0) + (opts.preview ? 1 : 0) !== 1) {
    // eslint-disable-next-line no-console
    console.error("Exactly one of --url or --preview is required");
    usage(1);
  }

  const envIterations = Number.parseInt(process.env.PERF_ITERATIONS ?? "", 10);
  const iterations = opts.iterations ?? (Number.isFinite(envIterations) ? envIterations : null) ?? 3;
  if (!Number.isFinite(iterations) || iterations <= 0) {
    // eslint-disable-next-line no-console
    console.error("--iterations must be a positive integer (or set PERF_ITERATIONS)");
    usage(1);
  }

  const envPreviewPort = Number.parseInt(process.env.PERF_PREVIEW_PORT ?? "", 10);
  const previewPort = opts.previewPort ?? (Number.isFinite(envPreviewPort) ? envPreviewPort : null) ?? 4173;
  if (opts.preview && (!Number.isFinite(previewPort) || previewPort <= 0 || previewPort > 65535)) {
    // eslint-disable-next-line no-console
    console.error("--preview-port must be a valid TCP port");
    usage(1);
  }

  const parseBool = (value) => {
    if (value == null) return null;
    const v = String(value).trim().toLowerCase();
    if (!v) return null;
    if (["1", "true", "yes", "y", "on"].includes(v)) return true;
    if (["0", "false", "no", "n", "off"].includes(v)) return false;
    return null;
  };

  const envTrace = parseBool(process.env.PERF_TRACE);
  const envTraceDuration = Number.parseInt(process.env.PERF_TRACE_DURATION_MS ?? "", 10);
  const traceDurationMs =
    opts.traceDurationMs ?? (Number.isFinite(envTraceDuration) && envTraceDuration > 0 ? envTraceDuration : null);
  if (traceDurationMs !== null && (!Number.isFinite(traceDurationMs) || traceDurationMs <= 0)) {
    // eslint-disable-next-line no-console
    console.error("--trace-duration-ms must be a positive integer (or set PERF_TRACE_DURATION_MS)");
    usage(1);
  }

  // PERF_TRACE_DURATION_MS implies trace capture unless PERF_TRACE is explicitly set to "0"/false.
  let trace = opts.trace ?? envTrace;
  if (trace === null) trace = traceDurationMs !== null;
  trace = Boolean(trace);

  const envIncludeAeroBench = parseBool(process.env.PERF_INCLUDE_AERO_BENCH);
  const includeAeroBench = opts.includeAeroBench ?? envIncludeAeroBench ?? false;

  return {
    workspace: opts.workspace,
    url: opts.url,
    preview: opts.preview,
    previewPort,
    perfRunner: opts.perfRunner,
    trace,
    traceDurationMs,
    includeAeroBench,
    outDir: opts.outDir,
    iterations,
  };
}

function npmCmd() {
  return process.platform === "win32" ? "npm.cmd" : "npm";
}

function waitForExit(child, { timeoutMs }) {
  if (child.exitCode !== null || child.signalCode) {
    return Promise.resolve({ code: child.exitCode, signal: child.signalCode ?? null });
  }

  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      reject(new Error(`Timed out waiting for process ${child.pid} to exit`));
    }, timeoutMs);

    child.once("exit", (code, signal) => {
      clearTimeout(timer);
      resolve({ code, signal });
    });
    child.once("error", (err) => {
      clearTimeout(timer);
      reject(err);
    });
  });
}

async function execChecked(cmd, args, { cwd, env, label }) {
  // eslint-disable-next-line no-console
  console.log(`[ci-perf] ${label}: ${cmd} ${args.join(" ")}`);

  const child = spawn(cmd, args, {
    cwd,
    env,
    stdio: "inherit",
  });

  const { code, signal } = await waitForExit(child, { timeoutMs: 30 * 60_000 });

  if (code !== 0) {
    throw new Error(`${label} failed (exit ${code ?? "null"}, signal ${signal ?? "null"})`);
  }
}

async function fetchOk(url) {
  const res = await fetch(url, { redirect: "manual" });
  return res.status > 0 && res.status < 400;
}

async function sleep(ms) {
  await new Promise((resolve) => setTimeout(resolve, ms));
}

async function waitForHttpReady(url, { timeoutMs, intervalMs, serverProcess, getServerError }) {
  function formatUrlForError(u) {
    try {
      const parsed = new URL(String(u));
      return `${parsed.origin}${parsed.pathname}`;
    } catch {
      const s = String(u);
      return s.length > 128 ? `${s.slice(0, 128)}…(${s.length} chars)` : s;
    }
  }

  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    if (typeof getServerError === "function") {
      const err = getServerError();
      if (err) throw err;
    }

    if (serverProcess && (serverProcess.exitCode !== null || serverProcess.signalCode)) {
      throw new Error(
        `Preview server exited early (code ${serverProcess.exitCode ?? "null"}, signal ${serverProcess.signalCode ?? "null"})`,
      );
    }

    try {
      if (await fetchOk(url)) return;
    } catch {
      // ignore
    }
    await sleep(intervalMs);
  }

  // Final attempt with a real error.
  if (!(await fetchOk(url))) {
    throw new Error(`Timed out waiting for ${formatUrlForError(url)} to become ready`);
  }
}

async function canBind(port) {
  return new Promise((resolve) => {
    const srv = net.createServer();
    unrefBestEffort(srv);
    srv.once("error", () => resolve(false));
    srv.listen({ port, host: "127.0.0.1" }, () => {
      srv.close(() => resolve(true));
    });
  });
}

async function ensurePortFree(port) {
  if (await canBind(port)) return port;
  if (await canBind(0)) {
    // If requested port is occupied, fall back to an ephemeral port.
    // eslint-disable-next-line no-console
    console.warn(`[ci-perf] preview port ${port} is in use; falling back to an ephemeral port`);
    return new Promise((resolve, reject) => {
      const srv = net.createServer();
      unrefBestEffort(srv);
      srv.once("error", (err) => reject(err));
      srv.listen({ port: 0, host: "127.0.0.1" }, () => {
        const addr = srv.address();
        if (!addr || typeof addr === "string") {
          srv.close(() => reject(new Error("Failed to allocate ephemeral port")));
          return;
        }
        const p = addr.port;
        srv.close(() => resolve(p));
      });
    });
  }
  return port;
}

async function stopPreviewServer(server) {
  if (!server) return;

  const pid = server.pid;
  if (!pid) return;

  const killPid = (targetPid, sig) => {
    try {
      process.kill(targetPid, sig);
    } catch {
      // ignore
    }
  };

  // Best-effort: terminate the full process group on POSIX (Vite spawns child processes).
  if (process.platform !== "win32") {
    killPid(-pid, "SIGTERM");
  }
  killPid(pid, "SIGTERM");

  try {
    await waitForExit(server, { timeoutMs: 5_000 });
    return;
  } catch {
    // ignore
  }

  if (process.platform !== "win32") {
    killPid(-pid, "SIGKILL");
  }
  killPid(pid, "SIGKILL");

  try {
    await waitForExit(server, { timeoutMs: 5_000 });
  } catch {
    // ignore
  }
}

async function assertOutputLayout(outDirAbs) {
  const required = ["raw.json", "summary.json"];
  for (const f of required) {
    const p = path.join(outDirAbs, f);
    try {
      await fs.access(p);
    } catch {
      throw new Error(`[ci-perf] expected output file missing: ${p}`);
    }
  }

  // Some versions of `tools/perf/run.mjs` may not write these files yet; keep
  // artifact layout stable by creating placeholders.
  const placeholders = ["perf_export.json", "trace.json"];
  for (const f of placeholders) {
    const p = path.join(outDirAbs, f);
    try {
      await fs.access(p);
    } catch {
      await fs.writeFile(p, "null\n");
    }
  }
}

async function main() {
  const opts = parseArgs(process.argv.slice(2));

  const workspaceAbs = path.resolve(process.cwd(), opts.workspace);
  const outDirAbs = path.resolve(process.cwd(), opts.outDir);
  await fs.mkdir(outDirAbs, { recursive: true });

  const scriptPath = fileURLToPath(import.meta.url);
  const scriptDir = path.dirname(scriptPath);
  const repoRoot = path.resolve(scriptDir, "../..");
  const perfRunner = opts.perfRunner
    ? path.resolve(process.cwd(), opts.perfRunner)
    : path.join(repoRoot, "tools", "perf", "run.mjs");
  try {
    await fs.access(perfRunner);
  } catch {
    throw new Error(`[ci-perf] perf runner not found at ${perfRunner}`);
  }

  let server = null;
  const cleanup = async () => {
    await stopPreviewServer(server);
    server = null;
  };

  const onSignal = (sig) => {
    // eslint-disable-next-line no-console
    console.error(`[ci-perf] received ${sig}, cleaning up...`);
    cleanup().finally(() => process.exit(1));
  };
  process.once("SIGINT", onSignal);
  process.once("SIGTERM", onSignal);

  try {
    let url = opts.url;

    if (opts.preview) {
      const port = await ensurePortFree(opts.previewPort);
      const host = "127.0.0.1";

      await execChecked(npmCmd(), ["run", "build"], { cwd: workspaceAbs, env: process.env, label: "build" });

      server = spawn(
        npmCmd(),
        ["run", "preview", "--", "--host", host, "--port", String(port), "--strictPort"],
        {
          cwd: workspaceAbs,
          env: process.env,
          stdio: "inherit",
          detached: process.platform !== "win32",
        },
      );
      let serverSpawnError = null;
      server.once("error", (err) => {
        serverSpawnError = err;
      });

      url = `http://${host}:${port}/`;
      await waitForHttpReady(url, {
        timeoutMs: 60_000,
        intervalMs: 1_000,
        serverProcess: server,
        getServerError: () => serverSpawnError,
      });
    }

    const runnerArgs = [perfRunner, "--url", url, "--out-dir", outDirAbs, "--iterations", String(opts.iterations)];
    if (opts.trace) {
      runnerArgs.push("--trace");
      if (opts.traceDurationMs !== null) {
        runnerArgs.push("--trace-duration-ms", String(opts.traceDurationMs));
      }
    }
    if (opts.includeAeroBench) {
      runnerArgs.push("--include-aero-bench");
    }

    await execChecked(process.execPath, runnerArgs, {
      cwd: workspaceAbs,
      env: process.env,
      label: "tools/perf/run.mjs",
    });

    await assertOutputLayout(outDirAbs);
  } finally {
    await cleanup();
  }
}

await main();
