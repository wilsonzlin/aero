import { execFileSync, spawnSync } from "node:child_process";
import { fail } from "./github_io.mjs";

export function actionTimeoutMs(defaultMs) {
  const raw = (process.env.AERO_ACTION_TIMEOUT_MS ?? "").trim();
  if (!raw) return defaultMs;
  const n = Number.parseInt(raw, 10);
  if (!Number.isFinite(n) || n <= 0) return defaultMs;
  return n;
}

export function spawnSyncChecked(command, args, options) {
  const res = spawnSync(command, args, { encoding: "utf8", ...options });
  if (res.error) {
    if (res.error.code === "ETIMEDOUT") {
      fail(`error: command timed out: ${command} ${args.join(" ")}`);
    }
    fail(`error: failed to spawn: ${command} ${args.join(" ")}\n${String(res.error)}`);
  }
  if ((res.status ?? 1) !== 0) {
    const details = (res.stderr || res.stdout || "").trim();
    fail(`error: command failed: ${command} ${args.join(" ")}\n${details}`);
  }
  return res;
}

export function execFileSyncUtf8(command, args, options) {
  try {
    return execFileSync(command, args, { encoding: "utf8", ...options });
  } catch (err) {
    const message = err && typeof err === "object" && "message" in err ? String(err.message) : String(err);
    fail(`error: command failed: ${command} ${args.join(" ")}\n${message}`);
  }
}

export function execNodeCliUtf8(args, options) {
  if (!Array.isArray(args) || args.length === 0) {
    fail("error: execNodeCliUtf8 requires a non-empty args array");
  }
  return execFileSyncUtf8(process.execPath, args, options);
}

export function execNodeCliInherit(args, options) {
  if (!Array.isArray(args) || args.length === 0) {
    fail("error: execNodeCliInherit requires a non-empty args array");
  }
  const res = spawnSync(process.execPath, args, { stdio: "inherit", ...options });
  if (res.error) {
    if (res.error.code === "ETIMEDOUT") {
      fail(`error: command timed out: node ${args.join(" ")}`);
    }
    fail(`error: failed to spawn: node ${args.join(" ")}\n${String(res.error)}`);
  }
  if ((res.status ?? 1) !== 0) {
    fail(`error: command failed: node ${args.join(" ")}`);
  }
}

