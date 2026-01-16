#!/usr/bin/env node
/**
 * Resolve Aero's cross-tooling environment variables into a normalized form.
 *
 * This script is intentionally dependency-free and is usable from both:
 * - bash: `eval "$(node scripts/env/resolve.mjs --format bash --require-node-dir)"`
 * - node: `const cfg = JSON.parse(execFileSync('node', ['scripts/env/resolve.mjs']))`
 */

import { execFileSync, spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import os from "node:os";
import { fileURLToPath } from "node:url";
import { formatOneLineError } from "../../src/text.js";

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(scriptDir, "../..");

function die(message) {
  console.error(`error: ${message}`);
  process.exit(1);
}

function warn(message) {
  console.error(`warning: ${message}`);
}

function usage() {
  console.log(`Usage: node scripts/env/resolve.mjs [options]

Options:
  --format <json|bash>        Output format (default: json)
  --print <key>               Print a single resolved key (e.g. AERO_NODE_DIR_REL)

  --node-dir <path>           Override AERO_NODE_DIR
  --wasm-crate-dir <path>     Override AERO_WASM_CRATE_DIR

  --require-node-dir          Fail if a node workspace directory cannot be resolved
  --require-wasm-crate-dir    Fail if a wasm crate directory cannot be resolved

  --require-webgpu, --webgpu  Force AERO_REQUIRE_WEBGPU=1
  --no-require-webgpu, --no-webgpu
                              Force AERO_REQUIRE_WEBGPU=0

  --disable-wgpu-texture-compression
                              Force AERO_DISABLE_WGPU_TEXTURE_COMPRESSION=1
  --no-disable-wgpu-texture-compression
                              Force AERO_DISABLE_WGPU_TEXTURE_COMPRESSION=0

Examples:
  node scripts/env/resolve.mjs --format json
  eval "$(node scripts/env/resolve.mjs --format bash --require-node-dir)"
`);
}

function coerceScalarString(value) {
  if (value == null) return null;
  switch (typeof value) {
    case "string":
      return value;
    case "number":
    case "boolean":
    case "bigint":
    case "symbol":
      return String(value);
    case "undefined":
    case "object":
    case "function":
    default:
      return null;
  }
}

function parseBoolean(name, raw, defaultValue) {
  const input = coerceScalarString(raw);
  if (input === null || input.trim() === "") return defaultValue;
  const v = input.trim().toLowerCase();
  if (v === "1" || v === "true" || v === "yes" || v === "on") return true;
  if (v === "0" || v === "false" || v === "no" || v === "off") return false;
  die(`${name} must be a boolean (1/0/true/false/yes/no/on/off), got: ${input}`);
}

function parseUrl(name, raw) {
  const input = coerceScalarString(raw);
  if (input === null) return null;
  const trimmed = input.trim();
  if (!trimmed) return null;
  try {
    // eslint-disable-next-line no-new
    new URL(trimmed);
    return trimmed;
  } catch (err) {
    die(`${name} must be a valid URL, got: ${trimmed} (${formatOneLineError(err, 256)})`);
  }
}

function parseCsvUrls(name, raw) {
  const input = coerceScalarString(raw);
  if (input === null) return [];
  const trimmed = input.trim();
  if (!trimmed) return [];
  const parts = trimmed
    .split(",")
    .map((p) => p.trim())
    .filter(Boolean);
  for (const p of parts) parseUrl(name, p);
  return parts;
}

function resolvePathMaybeAbs(input) {
  if (!input) return null;
  return path.isAbsolute(input) ? path.normalize(input) : path.resolve(repoRoot, input);
}

function toRelIfInsideRepo(absPath) {
  const rel = path.relative(repoRoot, absPath);
  if (!rel) return ".";
  if (rel.startsWith("..") || path.isAbsolute(rel)) return null;
  return rel;
}

function fileExists(p) {
  try {
    return fs.statSync(p).isFile();
  } catch {
    return false;
  }
}

function dirExists(p) {
  try {
    return fs.statSync(p).isDirectory();
  } catch {
    return false;
  }
}

function resolveDirFromEnv({ canonical, aliases }) {
  const canonicalValue = process.env[canonical];
  const aliasValues = aliases
    .map((alias) => ({ alias, value: process.env[alias] }))
    .filter((entry) => typeof entry.value === "string" && entry.value.trim() !== "");

  if (typeof canonicalValue === "string" && canonicalValue.trim() !== "") {
    // Canonical wins, but warn if deprecated aliases are also set (they will be ignored).
    for (const { alias } of aliasValues) {
      warn(`${alias} is deprecated and ignored because ${canonical} is set`);
    }
    return { value: canonicalValue, source: canonical };
  }

  if (aliasValues.length > 0) {
    const { alias, value } = aliasValues[0];
    warn(`${alias} is deprecated; use ${canonical} instead`);
    return { value, source: alias };
  }

  return null;
}

function autoDetectNodeDir() {
  const detectScript = path.resolve(repoRoot, "scripts/ci/detect-node-dir.mjs");
  if (fileExists(detectScript)) {
    const tmpOut = path.join(os.tmpdir(), `aero-detect-node-dir-${process.pid}-${Date.now()}.txt`);
    const result = spawnSync(process.execPath, [detectScript, "--allow-missing", "--github-output", tmpOut], {
      cwd: repoRoot,
      stdio: ["ignore", "pipe", "pipe"],
      encoding: "utf8",
    });
    if ((result.status ?? 1) !== 0) {
      const details = (result.stderr || result.stdout || "").trim();
      die(`detect-node-dir failed.\n\n${details}`);
    }

    for (const line of (result.stdout || "").split(/\r?\n/u)) {
      const idx = line.indexOf("=");
      if (idx === -1) continue;
      const key = line.slice(0, idx).trim();
      const value = line.slice(idx + 1).trim();
      if (key === "dir" && value) {
        return resolvePathMaybeAbs(value);
      }
    }
    return null;
  }

  const candidates = [".", "frontend", "web"];
  for (const rel of candidates) {
    const abs = path.resolve(repoRoot, rel);
    if (fileExists(path.join(abs, "package.json"))) return abs;
  }
  return null;
}

function resolveNodeDir({ override }) {
  let raw = override;
  let source = "cli";
  if (!raw) {
    const fromEnv = resolveDirFromEnv({
      canonical: "AERO_NODE_DIR",
      aliases: ["AERO_WEB_DIR", "WEB_DIR"],
    });
    if (fromEnv) {
      raw = fromEnv.value;
      source = fromEnv.source;
    }
  }

  let abs = raw ? resolvePathMaybeAbs(raw) : autoDetectNodeDir();
  if (!abs) return { abs: null, rel: null, source: source === "cli" ? "auto" : source };

  abs = path.normalize(abs);

  if (!dirExists(abs)) {
    die(`${source} points to a missing directory: ${abs}`);
  }
  if (!fileExists(path.join(abs, "package.json"))) {
    die(`package.json not found in node dir: ${abs} (set AERO_NODE_DIR to a directory containing package.json)`);
  }

  return { abs, rel: toRelIfInsideRepo(abs), source };
}

function resolveWasmCrateDir({ override, require }) {
  let raw = override;
  let source = "cli";
  if (!raw) {
    const fromEnv = resolveDirFromEnv({
      canonical: "AERO_WASM_CRATE_DIR",
      aliases: ["AERO_WASM_DIR", "WASM_CRATE_DIR"],
    });
    if (fromEnv) {
      raw = fromEnv.value;
      source = fromEnv.source;
    }
  }

  const resolverScript = path.join(repoRoot, "scripts/ci/detect-wasm-crate.mjs");
  if (!fileExists(resolverScript)) {
    die(`detect-wasm-crate script not found at ${resolverScript}`);
  }

  const cmdArgs = [resolverScript];
  if (!require && !raw) {
    cmdArgs.push("--allow-missing");
  }
  if (raw) {
    cmdArgs.push("--wasm-crate-dir", raw);
  }

  let stdout;
  try {
    stdout = execFileSync("node", cmdArgs, { cwd: repoRoot, encoding: "utf8", stdio: ["ignore", "pipe", "inherit"] });
  } catch (err) {
    if (!require && !raw) {
      // If wasm is not required for the calling script, keep this helper forgiving.
      // (The canonical WASM build/test entrypoints will still surface a hard error
      // when/if wasm is actually requested.)
      return { abs: null, rel: null, source: source === "cli" ? "auto" : source };
    }
    die(`failed to resolve wasm crate dir: ${formatOneLineError(err, 256)}`);
  }

  let dirRelOrAbs = null;
  const stdoutText = typeof stdout === "string" ? stdout : "";
  for (const line of stdoutText.split(/\r?\n/u)) {
    const trimmed = line.trim();
    if (!trimmed) continue;
    const idx = trimmed.indexOf("=");
    if (idx === -1) continue;
    const key = trimmed.slice(0, idx);
    const value = trimmed.slice(idx + 1);
    if (key === "dir") {
      dirRelOrAbs = value;
      break;
    }
  }

  if (!dirRelOrAbs) {
    return { abs: null, rel: null, source: source === "cli" ? "auto" : source };
  }

  const abs = path.isAbsolute(dirRelOrAbs) ? dirRelOrAbs : path.resolve(repoRoot, dirRelOrAbs);
  return { abs, rel: toRelIfInsideRepo(abs), source };
}

function detectNodeLockfile(nodeDirAbs) {
  if (!nodeDirAbs) return null;
  const lockAbs = path.join(nodeDirAbs, "package-lock.json");
  return fileExists(lockAbs) ? lockAbs : null;
}

function parseArgs(argv) {
  const out = {
    format: "json",
    printKey: null,
    nodeDir: null,
    wasmCrateDir: null,
    requireNodeDir: false,
    requireWasmCrateDir: false,
    requireWebgpuOverride: null, // boolean | null
    disableWgpuTextureCompressionOverride: null, // boolean | null
  };

  for (let i = 2; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === "-h" || arg === "--help") {
      usage();
      process.exit(0);
    }

    if (arg === "--format") {
      out.format = argv[++i] ?? "";
      continue;
    }
    if (arg.startsWith("--format=")) {
      out.format = arg.slice("--format=".length);
      continue;
    }

    if (arg === "--print") {
      out.printKey = argv[++i] ?? "";
      continue;
    }
    if (arg.startsWith("--print=")) {
      out.printKey = arg.slice("--print=".length);
      continue;
    }

    if (arg === "--node-dir") {
      out.nodeDir = argv[++i] ?? "";
      continue;
    }
    if (arg.startsWith("--node-dir=")) {
      out.nodeDir = arg.slice("--node-dir=".length);
      continue;
    }

    if (arg === "--wasm-crate-dir") {
      out.wasmCrateDir = argv[++i] ?? "";
      continue;
    }
    if (arg.startsWith("--wasm-crate-dir=")) {
      out.wasmCrateDir = arg.slice("--wasm-crate-dir=".length);
      continue;
    }

    if (arg === "--require-node-dir") {
      out.requireNodeDir = true;
      continue;
    }
    if (arg === "--require-wasm-crate-dir") {
      out.requireWasmCrateDir = true;
      continue;
    }

    if (arg === "--require-webgpu" || arg === "--webgpu") {
      out.requireWebgpuOverride = true;
      continue;
    }
    if (arg === "--no-require-webgpu" || arg === "--no-webgpu") {
      out.requireWebgpuOverride = false;
      continue;
    }

    if (arg === "--disable-wgpu-texture-compression") {
      out.disableWgpuTextureCompressionOverride = true;
      continue;
    }
    if (arg === "--no-disable-wgpu-texture-compression") {
      out.disableWgpuTextureCompressionOverride = false;
      continue;
    }

    die(`unknown argument: ${arg}`);
  }

  if (out.format !== "json" && out.format !== "bash") {
    die(`unsupported --format: ${out.format} (expected json or bash)`);
  }
  if (out.printKey !== null && out.printKey.trim() === "") {
    die("--print requires a key name");
  }
  if (out.nodeDir !== null && out.nodeDir.trim() === "") out.nodeDir = null;
  if (out.wasmCrateDir !== null && out.wasmCrateDir.trim() === "") out.wasmCrateDir = null;

  return out;
}

function bashQuote(value) {
  const s = coerceScalarString(value) ?? "";
  return `'${s.replace(/'/g, `'\\''`)}'`;
}

const args = parseArgs(process.argv);

const requireWebgpu =
  args.requireWebgpuOverride ?? parseBoolean("AERO_REQUIRE_WEBGPU", process.env.AERO_REQUIRE_WEBGPU, false);
const disableWgpuTextureCompression =
  args.disableWgpuTextureCompressionOverride ??
  parseBoolean(
    "AERO_DISABLE_WGPU_TEXTURE_COMPRESSION",
    process.env.AERO_DISABLE_WGPU_TEXTURE_COMPRESSION,
    false,
  );
const viteDisableCoopCoep = parseBoolean(
  "VITE_DISABLE_COOP_COEP",
  process.env.VITE_DISABLE_COOP_COEP,
  false,
);

// Validate (optional) proxy/backend URL env vars so failures are actionable.
const webrtcRelayPublicBaseUrl = parseUrl(
  "AERO_WEBRTC_UDP_RELAY_PUBLIC_BASE_URL",
  process.env.AERO_WEBRTC_UDP_RELAY_PUBLIC_BASE_URL,
);
const stunUrls = parseCsvUrls("AERO_STUN_URLS", process.env.AERO_STUN_URLS);
const turnUrls = parseCsvUrls("AERO_TURN_URLS", process.env.AERO_TURN_URLS);

const nodeDir = resolveNodeDir({ override: args.nodeDir });
if (args.requireNodeDir && !nodeDir.abs) {
  die("unable to locate package.json; set AERO_NODE_DIR or pass --node-dir <path>");
}

const wasmCrateDir = resolveWasmCrateDir({ override: args.wasmCrateDir, require: args.requireWasmCrateDir });
if (args.requireWasmCrateDir && !wasmCrateDir.abs) {
  die("unable to locate a wasm crate (Cargo.toml); set AERO_WASM_CRATE_DIR or pass --wasm-crate-dir <path>");
}

// In an npm-workspaces monorepo, `package-lock.json` lives at the repo root even
// when callers point `AERO_NODE_DIR` at a workspace subdirectory (e.g. `web/`).
const nodeLockfileAbs = detectNodeLockfile(nodeDir.abs) ?? detectNodeLockfile(repoRoot);

const resolved = {
  // Useful for consumers that need to resolve relative paths.
  repoRoot,

  AERO_REQUIRE_WEBGPU: requireWebgpu ? "1" : "0",
  AERO_DISABLE_WGPU_TEXTURE_COMPRESSION: disableWgpuTextureCompression ? "1" : "0",
  VITE_DISABLE_COOP_COEP: viteDisableCoopCoep ? "1" : "0",

  AERO_NODE_DIR: nodeDir.abs ?? "",
  AERO_NODE_DIR_REL: nodeDir.rel ?? "",
  AERO_NODE_LOCKFILE: nodeLockfileAbs ? toRelIfInsideRepo(nodeLockfileAbs) ?? nodeLockfileAbs : "",

  AERO_WASM_CRATE_DIR: wasmCrateDir.abs ?? "",
  AERO_WASM_CRATE_DIR_REL: wasmCrateDir.rel ?? "",

  // Not currently consumed by core scripts, but validated here so callers can
  // rely on "resolve succeeded => URLs were well-formed".
  AERO_WEBRTC_UDP_RELAY_PUBLIC_BASE_URL: webrtcRelayPublicBaseUrl ?? "",
  AERO_STUN_URLS: stunUrls.join(","),
  AERO_TURN_URLS: turnUrls.join(","),
};

if (args.printKey) {
  if (!(args.printKey in resolved)) {
    die(
      `unknown key for --print: ${args.printKey} (known keys: ${Object.keys(resolved)
        .sort()
        .join(", ")})`,
    );
  }
  // eslint-disable-next-line no-console
  console.log(resolved[args.printKey]);
  process.exit(0);
}

if (args.format === "json") {
  // eslint-disable-next-line no-console
  console.log(JSON.stringify(resolved, null, 2));
  process.exit(0);
}

// bash format: print `export KEY='value'` lines.
for (const [k, v] of Object.entries(resolved)) {
  if (k === "repoRoot") continue;
  // eslint-disable-next-line no-console
  console.log(`export ${k}=${bashQuote(v)}`);
}
