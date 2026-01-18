import { spawn, spawnSync } from "node:child_process";
import { once } from "node:events";
import { access, mkdir, readFile, rm, stat, writeFile } from "node:fs/promises";
import path from "node:path";
import { setTimeout as sleep } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import { unrefBestEffort } from "../src/unref_safe.js";

const REPO_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
// Keep enough stdout/stderr to surface useful errors without risking runaway logs in CI.
const COMMAND_OUTPUT_LIMIT = 200_000;
// Keep the spawned `cargo build` timeout below the Node test timeouts so we fail with an
// actionable error (and kill the build process) instead of letting the Node test runner time out
// while Cargo is still running.
const BUILD_TIMEOUT_MS = 12 * 60_000;
const BUILD_LOCK_TIMEOUT_MS = BUILD_TIMEOUT_MS + 2 * 60_000;
const BUILD_LOCK_RETRY_MS = 200;
const BUILD_STAMP_TIMEOUT_MS = 5_000;

function parsePositiveIntEnv(value) {
  if (typeof value !== "string") return null;
  if (!/^[1-9][0-9]*$/.test(value)) return null;
  const n = Number.parseInt(value, 10);
  if (!Number.isSafeInteger(n) || n <= 0) return null;
  return n;
}

let rustcHostTargetCache;
function rustcHostTarget() {
  if (rustcHostTargetCache !== undefined) return rustcHostTargetCache;
  try {
    const vv = spawnSync("rustc", ["-vV"], {
      cwd: REPO_ROOT,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "ignore"],
      timeout: 5_000,
    });
    if (vv.status !== 0) {
      rustcHostTargetCache = null;
      return rustcHostTargetCache;
    }
    const m = (vv.stdout ?? "").match(/^host:\s*(.+)\s*$/m);
    rustcHostTargetCache = m ? m[1].trim() : null;
    return rustcHostTargetCache;
  } catch {
    rustcHostTargetCache = null;
    return rustcHostTargetCache;
  }
}

function cargoTargetRustflagsVar(target) {
  // Cargo reads per-target rustflags env vars in the form:
  //   CARGO_TARGET_<TRIPLE>_RUSTFLAGS
  // with '-' and '.' replaced by '_', and the triple uppercased.
  return `CARGO_TARGET_${target.toUpperCase().replace(/[-.]/g, "_")}_RUSTFLAGS`;
}

function isSccacheWrapper(value) {
  if (!value) return false;
  const v = value.toLowerCase();
  return (
    v === "sccache" ||
    v === "sccache.exe" ||
    v.endsWith("/sccache") ||
    v.endsWith("\\sccache") ||
    v.endsWith("/sccache.exe") ||
    v.endsWith("\\sccache.exe")
  );
}

function readBuildStamp() {
  try {
    const rev = spawnSync("git", ["rev-parse", "HEAD"], {
      cwd: REPO_ROOT,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "ignore"],
      timeout: BUILD_STAMP_TIMEOUT_MS,
    });
    if (rev.status !== 0) return null;
    const sha = (rev.stdout ?? "").trim();
    if (!sha) return null;

    const status = spawnSync("git", ["status", "--porcelain"], {
      cwd: REPO_ROOT,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "ignore"],
      timeout: BUILD_STAMP_TIMEOUT_MS,
    });
    if (status.status !== 0) return null;
    // If the working tree is dirty, don't attempt to use the cached stamp/binary.
    if ((status.stdout ?? "").trim() !== "") return null;

    return sha;
  } catch {
    return null;
  }
}

function appendLimitedOutput(prev, chunk) {
  let out = prev + chunk.toString("utf8");
  if (out.length > COMMAND_OUTPUT_LIMIT) out = out.slice(-COMMAND_OUTPUT_LIMIT);
  return out;
}

function signalProcessTree(child, signal) {
  if (!child.pid) return;
  // `detached: true` makes the child process leader of a new process group (POSIX).
  // Kill the group so `cargo build` doesn't leak rustc processes on timeouts.
  if (process.platform !== "win32") {
    try {
      process.kill(-child.pid, signal);
      return;
    } catch {
      // Fall back to killing the main pid.
    }
  }
  try {
    child.kill(signal);
  } catch {
    // ignore
  }
}

async function withBuildLock(fn) {
  const targetDir = await getCargoTargetDir();
  await mkdir(targetDir, { recursive: true });
  const lockDir = path.join(targetDir, ".aero-l2-proxy-build.lock");
  const lockOwnerPath = path.join(lockDir, "owner.json");
  const start = Date.now();

  while (true) {
    try {
      await mkdir(lockDir);
      try {
        await writeFile(lockOwnerPath, JSON.stringify({ pid: process.pid, startedAt: Date.now() }), "utf8");
      } catch (err) {
        await rm(lockDir, { recursive: true, force: true });
        throw err;
      }
      break;
    } catch (err) {
      if (!err || typeof err !== "object" || err.code !== "EEXIST") throw err;
      if (Date.now() - start > BUILD_LOCK_TIMEOUT_MS) {
        throw new Error(`timeout waiting for aero-l2-proxy build lock (${lockDir})`);
      }

      // If the worker holding the lock died (e.g. test runner SIGKILL), clean up the lock dir so
      // other test processes can proceed.
      let cleaned = false;
      try {
        const raw = await readFile(lockOwnerPath, "utf8");
        const parsed = JSON.parse(raw);
        const pid = Number(parsed?.pid ?? 0);
        if (Number.isFinite(pid) && pid > 0) {
          let alive = true;
          try {
            process.kill(pid, 0);
          } catch (probeErr) {
            // `process.kill(pid, 0)` throws `ESRCH` when the process doesn't exist.
            alive = !(probeErr && typeof probeErr === "object" && probeErr.code === "ESRCH");
          }
          if (!alive) {
            await rm(lockDir, { recursive: true, force: true });
            cleaned = true;
          }
        }
      } catch {
        // If we can't read/parse the owner file, fall back to an age-based cleanup so a corrupt lock
        // doesn't wedge tests forever.
        try {
          const info = await stat(lockDir);
          if (Date.now() - info.mtimeMs > BUILD_LOCK_TIMEOUT_MS) {
            await rm(lockDir, { recursive: true, force: true });
            cleaned = true;
          }
        } catch {
          // ignore
        }
      }

      if (cleaned) continue;
      await sleep(BUILD_LOCK_RETRY_MS);
    }
  }

  try {
    return await fn();
  } finally {
    await rm(lockDir, { recursive: true, force: true });
  }
}

let cargoTargetDirPromise;
async function getCargoTargetDir() {
  if (cargoTargetDirPromise) return cargoTargetDirPromise;
  cargoTargetDirPromise = (async () => {
    // `cargo metadata` output can be very large in this repo, and we only need the
    // (potentially overridden) target directory.
    const targetDir = process.env.CARGO_TARGET_DIR;
    if (targetDir) {
      return path.isAbsolute(targetDir) ? targetDir : path.join(REPO_ROOT, targetDir);
    }
    // Build into a dedicated subdirectory to avoid contending with other cargo builds
    // that use the default workspace target dir.
    return path.join(REPO_ROOT, "target", "l2-proxy-node-tests");
  })();
  return cargoTargetDirPromise;
}

async function getProxyBinPath() {
  const targetDir = await getCargoTargetDir();
  const exe = process.platform === "win32" ? "aero-l2-proxy.exe" : "aero-l2-proxy";
  return path.join(targetDir, "debug", exe);
}

let buildPromise;
async function ensureProxyBuilt() {
  if (buildPromise) return buildPromise;
  buildPromise = (async () => {
    const binPath = await getProxyBinPath();
    const targetDir = await getCargoTargetDir();
    const stampPath = path.join(targetDir, ".aero-l2-proxy-build.stamp");
    const expectedStamp = readBuildStamp();

    const isUpToDate = async () => {
      if (!expectedStamp) return false;
      const exists = await access(binPath)
        .then(() => true)
        .catch(() => false);
      if (!exists) return false;
      const existing = await readFile(stampPath, "utf8")
        .then((s) => s.trim())
        .catch(() => null);
      return existing === expectedStamp;
    };

    // Fast path: we already built this binary for the current git SHA.
    if (await isUpToDate()) {
      return;
    }

    await withBuildLock(async () => {
      // Another worker may have completed the build while we were waiting.
      if (await isUpToDate()) {
        return;
      }

      const env = { CARGO_TARGET_DIR: targetDir };
      // Avoid global Cargo package cache locks when running concurrent CI jobs / agents.
      env.CARGO_HOME = process.env.AERO_L2_PROXY_TEST_CARGO_HOME ?? path.join(targetDir, "node-test-cargo-home");
      await mkdir(env.CARGO_HOME, { recursive: true });

      // Always invoke `cargo build` so the binary stays in sync with the checked out sources.
      // (Relying solely on the existence of the previous build output can lead to stale binaries
      // when the repo is updated between test runs.)
      const baseArgs = ["build", "--quiet", "--locked", "-p", "aero-l2-proxy"];
      const binExists = await access(binPath)
        .then(() => true)
        .catch(() => false);

      // If we already have a binary, prefer an offline build first to avoid unnecessary network
      // churn during repeated Node test runs.
      let triedOffline = false;
      if (binExists) {
        triedOffline = true;
        try {
          await runCommand("cargo", [...baseArgs, "--offline"], {
            cwd: REPO_ROOT,
            env,
            timeoutMs: BUILD_TIMEOUT_MS,
          });
        } catch {
          // Fall back to an online build (e.g. if the isolated CARGO_HOME is missing cached deps).
          triedOffline = false;
        }
      }

      if (!binExists || !triedOffline) {
        await runCommand("cargo", baseArgs, {
          cwd: REPO_ROOT,
          env,
          // A cold `cargo build` of the full dependency graph can be slow on CI runners.
          // Keep this bounded (deterministic) but generous enough to avoid flakes.
          timeoutMs: BUILD_TIMEOUT_MS,
        });
      }
      await access(binPath);

      if (expectedStamp) {
        try {
          await writeFile(stampPath, `${expectedStamp}\n`, "utf8");
        } catch {
          // Best-effort; a missing stamp only means we might rebuild next time.
        }
      }
    });
  })();
  try {
    await buildPromise;
  } catch (err) {
    buildPromise = null;
    throw err;
  }
  return buildPromise;
}

async function stopProcess(child) {
  if (child.exitCode !== null || child.signalCode !== null) return;

  // `aero-l2-proxy` listens for Ctrl+C (SIGINT) and performs a best-effort graceful shutdown.
  // Fall back to SIGKILL if it doesn't exit promptly.
  try {
    child.kill("SIGINT");
  } catch {
    try {
      child.kill();
    } catch {
      // ignore
    }
  }

  const exited = await Promise.race([once(child, "exit"), sleep(2_000, undefined, { ref: false }).then(() => null)]);
  if (exited !== null) return;

  try {
    child.kill("SIGKILL");
  } catch {
    try {
      child.kill();
    } catch {
      // ignore
    }
  }
  await once(child, "exit");
}

async function waitForListeningAddr(child) {
  const timeoutMs = 5_000;
  const listenRe = /aero-l2-proxy listening on http:\/\/(?<host>[^:\s]+):(?<port>\d+)/;

  let output = "";
  let stdoutBuf = "";
  let stderrBuf = "";

  return new Promise((resolve, reject) => {
    const timeout = setTimeout(() => {
      cleanup();
      reject(new Error(`timeout waiting for aero-l2-proxy to bind\n\n${output}`));
    }, timeoutMs);
    unrefBestEffort(timeout);

    const onChunk = (chunk, bufGetter, bufSetter) => {
      const text = chunk.toString("utf8");
      output += text;

      let buf = bufGetter();
      buf += text;

      const lines = buf.split(/\r?\n/);
      buf = lines.pop() ?? "";
      bufSetter(buf);

      for (const line of lines) {
        const m = line.match(listenRe);
        if (!m) continue;
        const port = Number(m.groups?.port);
        if (!Number.isSafeInteger(port)) continue;
        cleanup();
        resolve({ host: m.groups?.host ?? "127.0.0.1", port });
        return;
      }

      // Handle the common case where the "listening on ..." line arrives without a trailing newline.
      const m = buf.match(listenRe);
      if (m) {
        const port = Number(m.groups?.port);
        if (Number.isSafeInteger(port)) {
          cleanup();
          resolve({ host: m.groups?.host ?? "127.0.0.1", port });
        }
      }
    };

    const onStdout = (chunk) => onChunk(chunk, () => stdoutBuf, (v) => (stdoutBuf = v));
    const onStderr = (chunk) => onChunk(chunk, () => stderrBuf, (v) => (stderrBuf = v));

    const onExit = (code, signal) => {
      cleanup();
      reject(new Error(`aero-l2-proxy exited before binding (code=${code}, signal=${signal})\n\n${output}`));
    };

    const onError = (err) => {
      cleanup();
      reject(err);
    };

    const cleanup = () => {
      clearTimeout(timeout);
      child.off("exit", onExit);
      child.off("error", onError);
      child.stdout?.off("data", onStdout);
      child.stderr?.off("data", onStderr);
    };

    child.on("exit", onExit);
    child.on("error", onError);
    child.stdout?.on("data", onStdout);
    child.stderr?.on("data", onStderr);
  });
}

async function runCommand(command, args, { cwd, env, timeoutMs = 60_000 } = {}) {
  return new Promise((resolve, reject) => {
    let stdout = "";
    let stderr = "";

    const childEnv = {
      ...process.env,
      ...env,
    };

    if (command === "cargo") {
      // Defensive defaults: cap Cargo and rustc thread usage for environments with tight per-user
      // thread limits. This helper is invoked from Node tests that may run without `safe-run.sh`
      // and therefore would otherwise inherit Cargo's default parallelism (num_cpus).
      //
      // Prefer using the canonical agent knob `AERO_CARGO_BUILD_JOBS` when present; otherwise,
      // default to -j1 for reliability. Align rustc + Rayon thread pools with the chosen job
      // count to avoid rustc ICEs on EAGAIN/WouldBlock when spawning helper threads.
      const defaultJobs = 1;
      const jobsFromAero = parsePositiveIntEnv(childEnv.AERO_CARGO_BUILD_JOBS);
      const jobsFromCargo = parsePositiveIntEnv(childEnv.CARGO_BUILD_JOBS);
      const jobs = jobsFromAero ?? jobsFromCargo ?? defaultJobs;
      childEnv.CARGO_BUILD_JOBS = String(jobs);

      if (parsePositiveIntEnv(childEnv.RUSTC_WORKER_THREADS) === null) {
        childEnv.RUSTC_WORKER_THREADS = String(jobs);
      }
      if (parsePositiveIntEnv(childEnv.RAYON_NUM_THREADS) === null) {
        childEnv.RAYON_NUM_THREADS = String(jobs);
      }

      // Limit LLVM lld thread parallelism on Linux (matches safe-run/agent-env behavior).
      // Use Cargo's per-target rustflags env var rather than mutating global RUSTFLAGS so this
      // doesn't leak into wasm builds (rust-lld -flavor wasm does not understand -Wl,...).
      if (process.platform === "linux") {
        const host = rustcHostTarget();
        if (host) {
          const varName = cargoTargetRustflagsVar(host);
          const current = childEnv[varName] ?? "";
          if (!current.includes("--threads=") && !current.includes("-Wl,--threads=")) {
            childEnv[varName] = `${current} -C link-arg=-Wl,--threads=${jobs}`.trim();
          }
        }
      }

      // Prevent progress bars from spamming logs on CI timeouts.
      childEnv.CARGO_TERM_COLOR = "never";
      childEnv.CARGO_TERM_PROGRESS_WHEN = "never";

      // This helper is used from Node unit tests that spawn `cargo build`.
      // Some developer/CI environments enable a global rustc wrapper (most commonly `sccache`)
      // via environment variables. When the wrapper daemon/socket is unhealthy, Cargo can fail
      // before compiling anything. Detect `sccache` wrappers and override them.
      const wrapperVars = [
        "RUSTC_WRAPPER",
        "RUSTC_WORKSPACE_WRAPPER",
        "CARGO_BUILD_RUSTC_WRAPPER",
        "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER",
      ];
      const usesSccache = wrapperVars.some((k) => isSccacheWrapper(childEnv[k]));
      const hasWrapper = wrapperVars.some((k) => Object.prototype.hasOwnProperty.call(childEnv, k));
      if (usesSccache || !hasWrapper) {
        // Override user Cargo config (e.g. a global sccache wrapper) by explicitly disabling the
        // wrapper env vars. Setting an empty string disables the wrapper and takes precedence over
        // Cargo config.
        childEnv.RUSTC_WRAPPER = "";
        childEnv.RUSTC_WORKSPACE_WRAPPER = "";
        childEnv.CARGO_BUILD_RUSTC_WRAPPER = "";
        childEnv.CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER = "";
      }
    }

    const child = spawn(command, args, {
      cwd,
      env: childEnv,
      detached: process.platform !== "win32",
      stdio: ["ignore", "pipe", "pipe"],
    });

    let timingOut = false;
    const onStdout = (c) => {
      stdout = appendLimitedOutput(stdout, c);
    };
    const onStderr = (c) => {
      stderr = appendLimitedOutput(stderr, c);
    };
    child.stdout?.on("data", onStdout);
    child.stderr?.on("data", onStderr);

    let settled = false;
    const settle = (err, result) => {
      if (settled) return;
      settled = true;
      clearTimeout(timeout);
      child.stdout?.off("data", onStdout);
      child.stderr?.off("data", onStderr);
      err ? reject(err) : resolve(result);
    };

    const stopCommand = async (proc) => {
      if (proc.exitCode !== null || proc.signalCode !== null) return;

      const exitedPromise = once(proc, "exit").catch(() => null);
      signalProcessTree(proc, "SIGTERM");
      const exited = await Promise.race([
        exitedPromise,
        sleep(2_000, undefined, { ref: false }).then(() => null),
      ]);
      if (exited !== null) return;

      const killedPromise = once(proc, "exit").catch(() => null);
      signalProcessTree(proc, "SIGKILL");
      await killedPromise;
    };

    const timeout = setTimeout(() => {
      timingOut = true;
      child.off("exit", onExit);
      child.off("error", onError);

      const why = `command timed out after ${timeoutMs}ms: ${command} ${args.join(" ")}`;
      stopCommand(child)
        .then(() => settle(new Error(`${why}\n\n${stdout}${stderr}`)))
        .catch(() => settle(new Error(`${why}\n\n${stdout}${stderr}`)));
    }, timeoutMs);
    unrefBestEffort(timeout);

    const onError = (err) => {
      if (timingOut) return;
      settle(err);
    };

    const onExit = (code, signal) => {
      if (timingOut) return;
      if (code === 0) {
        settle(null, { stdout, stderr });
        return;
      }
      settle(
        new Error(
          `command failed: ${command} ${args.join(" ")} (code=${code}, signal=${signal})\n\n${stdout}${stderr}`,
        ),
      );
    };

    child.once("error", onError);
    child.once("exit", onExit);
  });
}

export async function startRustL2Proxy(env = {}) {
  await ensureProxyBuilt();
  const binPath = await getProxyBinPath();

  // Default Tokio runtime worker threads to match our conservative Cargo parallelism knob.
  // This keeps the spawned proxy reliable in thread-limited sandboxes where Tokio's default
  // (num_cpus) would spawn many worker threads.
  const defaultJobs = 1;
  const jobsFromAero = parsePositiveIntEnv(process.env.AERO_CARGO_BUILD_JOBS);
  const jobsFromCargo = parsePositiveIntEnv(process.env.CARGO_BUILD_JOBS);
  const jobs = jobsFromAero ?? jobsFromCargo ?? defaultJobs;

  const tokioFromEnv = parsePositiveIntEnv(process.env.AERO_TOKIO_WORKER_THREADS);
  const tokioThreads = tokioFromEnv ?? jobs;

  const child = spawn(binPath, [], {
    cwd: REPO_ROOT,
    env: {
      ...process.env,
      AERO_L2_PROXY_LISTEN_ADDR: "127.0.0.1:0",
      // Avoid inheriting origin allowlists from developer environments (these
      // can subtly change security behavior when tests rely on defaults).
      ALLOWED_ORIGINS: "",
      AERO_L2_ALLOWED_ORIGINS: "",
      AERO_L2_ALLOWED_ORIGINS_EXTRA: "",
      AERO_L2_ALLOWED_HOSTS: "",
      AERO_L2_TRUST_PROXY_HOST: "",
      // Avoid inheriting user auth-mode overrides from the test runner environment.
      AERO_L2_AUTH_MODE: "",
      AERO_L2_TOKEN: "",
      AERO_L2_API_KEY: "",
      AERO_L2_JWT_SECRET: "",
      AERO_L2_SESSION_SECRET: "",
      SESSION_SECRET: "",
      AERO_L2_OPEN: "",
      AERO_TOKIO_WORKER_THREADS: String(tokioThreads),
      ...env,
      // Ensure the "listening on ..." log line is emitted for test orchestration.
      RUST_LOG: "aero_l2_proxy=info",
    },
    stdio: ["ignore", "pipe", "pipe"],
  });

  const listen = await waitForListeningAddr(child);
  child.stdout?.resume();
  child.stderr?.resume();

  return {
    proc: child,
    port: listen.port,
    async close() {
      await stopProcess(child);
    },
  };
}
