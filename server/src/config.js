import path from "node:path";
import { fileURLToPath } from "node:url";
import ipaddr from "ipaddr.js";

const MAX_ENV_CSV_LEN = 64 * 1024;
const MAX_ENV_CSV_ITEMS = 1024;
const MAX_ENV_CSV_ITEM_LEN = 4 * 1024;
const MAX_ENV_NUMBER_LEN = 64;

const MAX_PORT = 65535;
const MAX_WS_MESSAGE_BYTES = 64 * 1024 * 1024;
const MAX_TCP_CONNECTIONS_PER_WS = 1024;
const MAX_TCP_CONNECTIONS_TOTAL = 100_000;
const MAX_WS_CONNECTIONS_PER_IP = 10_000;
const MAX_CONNECTS_PER_MINUTE = 1_000_000;
const MAX_BANDWIDTH_BPS = 1_000_000_000;

const DEFAULTS = Object.freeze({
  host: "0.0.0.0",
  port: 8080,
  allowPrivateRanges: false,
  maxTcpConnectionsPerWs: 8,
  maxTcpConnectionsTotal: 512,
  maxWsConnectionsPerIp: 4,
  bandwidthBytesPerSecond: 5_000_000,
  connectsPerMinute: 60,
  maxWsMessageBytes: 1_048_576,
  logLevel: "info",
});

function parseBoolean(value, defaultValue) {
  if (value == null) return defaultValue;
  const normalized = String(value).trim().toLowerCase();
  if (["1", "true", "yes", "y", "on"].includes(normalized)) return true;
  if (["0", "false", "no", "n", "off"].includes(normalized)) return false;
  return defaultValue;
}

function parseNumber(value, defaultValue, { min, max } = {}) {
  if (value == null) return defaultValue;
  const raw = String(value).trim();
  if (!raw) return defaultValue;
  if (raw.length > MAX_ENV_NUMBER_LEN) return defaultValue;
  if (!/^[+-]?\d+$/.test(raw)) return defaultValue;
  const num = Number.parseInt(raw, 10);
  if (!Number.isSafeInteger(num)) return defaultValue;
  const minValue = min ?? Number.MIN_SAFE_INTEGER;
  const maxValue = max ?? Number.MAX_SAFE_INTEGER;
  if (num < minValue || num > maxValue) return defaultValue;
  return num;
}

function parseCsv(value, name = "CSV") {
  if (!value) return [];
  const raw = String(value);
  if (raw.length > MAX_ENV_CSV_LEN) {
    throw new Error(`${name} is too long`);
  }

  /** @type {string[]} */
  const out = [];
  let start = 0;
  for (let i = 0; i <= raw.length; i++) {
    const ch = i === raw.length ? "," : raw[i];
    if (ch !== ",") continue;
    const token = raw.slice(start, i).trim();
    start = i + 1;
    if (!token) continue;
    if (token.length > MAX_ENV_CSV_ITEM_LEN) {
      throw new Error(`${name} contains an overly long item`);
    }
    out.push(token);
    if (out.length > MAX_ENV_CSV_ITEMS) {
      throw new Error(`${name} contains too many items`);
    }
  }
  return out;
}

export function parseAllowPorts(value) {
  if (!value) return [];
  const normalized = String(value).trim();
  if (normalized === "*") return [{ start: 1, end: 65535 }];
  const ranges = [];
  for (const part of parseCsv(normalized, "AERO_PROXY_ALLOW_PORTS")) {
    const m = /^(\d+)(?:-(\d+))?$/.exec(part);
    if (!m) throw new Error(`Invalid port allowlist entry: "${part}"`);
    const start = Number.parseInt(m[1], 10);
    const end = m[2] ? Number.parseInt(m[2], 10) : start;
    if (start < 1 || start > 65535 || end < 1 || end > 65535 || start > end) {
      throw new Error(`Invalid port range: "${part}"`);
    }
    ranges.push({ start, end });
  }
  return ranges;
}

export function parseAllowHosts(value) {
  if (!value) return [];
  const normalized = String(value).trim();
  if (normalized === "*") return [{ kind: "wildcard" }];

  const patterns = [];
  for (const part of parseCsv(normalized, "AERO_PROXY_ALLOW_HOSTS")) {
    if (part === "*") {
      patterns.push({ kind: "wildcard" });
      continue;
    }
    if (part.includes("/")) {
      const [addr, prefixLen] = ipaddr.parseCIDR(part);
      patterns.push({ kind: "cidr", addr, prefixLen });
      continue;
    }
    if (part.startsWith("*.")) {
      const suffix = part.slice(1).toLowerCase(); // includes leading "."
      if (suffix.length < 2) throw new Error(`Invalid host allowlist entry: "${part}"`);
      patterns.push({ kind: "suffix", suffix });
      continue;
    }
    patterns.push({ kind: "exact", value: part.toLowerCase() });
  }
  return patterns;
}

export function resolveConfig(overrides = {}, env = process.env) {
  const serverRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
  const staticDir = overrides.staticDir ?? env.AERO_PROXY_STATIC_DIR ?? path.join(serverRoot, "public");
  const tokensFromEnv = [
    ...(env.AERO_PROXY_TOKEN ? [String(env.AERO_PROXY_TOKEN)] : []),
    ...parseCsv(env.AERO_PROXY_TOKENS, "AERO_PROXY_TOKENS"),
  ];

  const tokens = overrides.tokens ?? tokensFromEnv;
  if (!tokens || tokens.length === 0) {
    throw new Error("AERO_PROXY_TOKEN is required (set env var or pass {tokens:[...]} in config)");
  }

  const allowHosts = overrides.allowHosts ?? parseAllowHosts(env.AERO_PROXY_ALLOW_HOSTS);
  const allowPorts = overrides.allowPorts ?? parseAllowPorts(env.AERO_PROXY_ALLOW_PORTS);
  const allowedOrigins =
    overrides.allowedOrigins ?? parseCsv(env.AERO_PROXY_ALLOWED_ORIGINS, "AERO_PROXY_ALLOWED_ORIGINS");

  return Object.freeze({
    host: overrides.host ?? env.AERO_PROXY_HOST ?? DEFAULTS.host,
    port: overrides.port ?? parseNumber(env.AERO_PROXY_PORT, DEFAULTS.port, { min: 0, max: MAX_PORT }),
    staticDir,
    tokens,
    allowHosts,
    allowPorts,
    allowedOrigins,
    allowPrivateRanges: overrides.allowPrivateRanges ?? parseBoolean(env.AERO_PROXY_ALLOW_PRIVATE_RANGES, DEFAULTS.allowPrivateRanges),
    maxTcpConnectionsPerWs:
      overrides.maxTcpConnectionsPerWs ??
      parseNumber(env.AERO_PROXY_MAX_TCP_PER_WS, DEFAULTS.maxTcpConnectionsPerWs, { min: 0, max: MAX_TCP_CONNECTIONS_PER_WS }),
    maxTcpConnectionsTotal:
      overrides.maxTcpConnectionsTotal ??
      parseNumber(env.AERO_PROXY_MAX_TCP_TOTAL, DEFAULTS.maxTcpConnectionsTotal, { min: 0, max: MAX_TCP_CONNECTIONS_TOTAL }),
    maxWsConnectionsPerIp:
      overrides.maxWsConnectionsPerIp ??
      parseNumber(env.AERO_PROXY_MAX_WS_PER_IP, DEFAULTS.maxWsConnectionsPerIp, { min: 0, max: MAX_WS_CONNECTIONS_PER_IP }),
    bandwidthBytesPerSecond:
      overrides.bandwidthBytesPerSecond ??
      parseNumber(env.AERO_PROXY_BANDWIDTH_BPS, DEFAULTS.bandwidthBytesPerSecond, { min: 0, max: MAX_BANDWIDTH_BPS }),
    connectsPerMinute:
      overrides.connectsPerMinute ??
      parseNumber(env.AERO_PROXY_CONNECTS_PER_MINUTE, DEFAULTS.connectsPerMinute, { min: 0, max: MAX_CONNECTS_PER_MINUTE }),
    maxWsMessageBytes:
      overrides.maxWsMessageBytes ??
      parseNumber(env.AERO_PROXY_MAX_WS_MESSAGE_BYTES, DEFAULTS.maxWsMessageBytes, { min: 1, max: MAX_WS_MESSAGE_BYTES }),
    logLevel: overrides.logLevel ?? env.AERO_PROXY_LOG_LEVEL ?? DEFAULTS.logLevel,
  });
}

