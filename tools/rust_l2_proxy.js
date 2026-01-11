import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { access } from "node:fs/promises";
import path from "node:path";
import { setTimeout as sleep } from "node:timers/promises";
import { fileURLToPath } from "node:url";

const REPO_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

let cargoTargetDirPromise;
async function getCargoTargetDir() {
  if (cargoTargetDirPromise) return cargoTargetDirPromise;
  cargoTargetDirPromise = (async () => {
    const { stdout } = await runCommand("cargo", ["metadata", "--format-version=1", "--no-deps"], {
      cwd: REPO_ROOT,
    });
    const meta = JSON.parse(stdout);
    assert.equal(typeof meta.target_directory, "string");
    return meta.target_directory;
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
    await runCommand("cargo", ["build", "--locked", "-p", "aero-l2-proxy"], { cwd: REPO_ROOT });
    const binPath = await getProxyBinPath();
    await access(binPath);
  })();
  return buildPromise;
}

async function stopProcess(child) {
  if (child.exitCode !== null || child.signalCode !== null) return;

  // `aero-l2-proxy` listens for Ctrl+C (SIGINT) and performs a best-effort graceful shutdown.
  // Fall back to SIGKILL if it doesn't exit promptly.
  try {
    child.kill("SIGINT");
  } catch {
    child.kill();
  }

  const exited = await Promise.race([once(child, "exit"), sleep(2_000).then(() => null)]);
  if (exited !== null) return;

  try {
    child.kill("SIGKILL");
  } catch {
    child.kill();
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
    timeout.unref();

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

async function runCommand(command, args, { cwd, env } = {}) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, {
      cwd,
      env,
      stdio: ["ignore", "pipe", "pipe"],
    });

    const stdoutChunks = [];
    const stderrChunks = [];
    child.stdout.on("data", (c) => stdoutChunks.push(c));
    child.stderr.on("data", (c) => stderrChunks.push(c));
    child.once("error", reject);
    child.once("exit", (code, signal) => {
      const stdout = Buffer.concat(stdoutChunks).toString("utf8");
      const stderr = Buffer.concat(stderrChunks).toString("utf8");
      if (code === 0) {
        resolve({ stdout, stderr });
        return;
      }
      reject(
        new Error(
          `command failed: ${command} ${args.join(" ")} (code=${code}, signal=${signal})\n\n${stdout}${stderr}`,
        ),
      );
    });
  });
}

export async function startRustL2Proxy(env = {}) {
  await ensureProxyBuilt();
  const binPath = await getProxyBinPath();

  const child = spawn(binPath, [], {
    cwd: REPO_ROOT,
    env: {
      ...process.env,
      AERO_L2_PROXY_LISTEN_ADDR: "127.0.0.1:0",
      ...env,
      // Ensure the "listening on ..." log line is emitted for test orchestration.
      RUST_LOG: "aero_l2_proxy=info",
    },
    stdio: ["ignore", "pipe", "pipe"],
  });

  const listen = await waitForListeningAddr(child);

  return {
    port: listen.port,
    async close() {
      await stopProcess(child);
    },
  };
}

