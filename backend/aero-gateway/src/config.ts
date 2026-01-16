import fs from 'node:fs';
import { z } from 'zod';
import { splitCommaSeparatedList } from './csv.js';
import { normalizeAllowedOriginString } from './security/origin.js';
import { formatOneLineUtf8 } from './util/text.js';
import {
  L2_TUNNEL_DEFAULT_MAX_CONTROL_PAYLOAD_BYTES,
  L2_TUNNEL_DEFAULT_MAX_FRAME_PAYLOAD_BYTES,
} from './protocol/l2Tunnel.js';

const logLevels = ['fatal', 'error', 'warn', 'info', 'debug', 'trace', 'silent'] as const;
const udpRelayAuthModes = ['none', 'api_key', 'jwt'] as const;

export type LogLevel = (typeof logLevels)[number];
export type UdpRelayAuthMode = (typeof udpRelayAuthModes)[number];

export type Config = Readonly<{
  HOST: string;
  PORT: number;
  LOG_LEVEL: LogLevel;
  ALLOWED_ORIGINS: string[];
  PUBLIC_BASE_URL: string;
  SHUTDOWN_GRACE_MS: number;
  CROSS_ORIGIN_ISOLATION: boolean;
  TRUST_PROXY: boolean;

  SESSION_SECRET: string;
  SESSION_TTL_SECONDS: number;
  SESSION_COOKIE_SAMESITE: 'Lax' | 'Strict' | 'None';

  RATE_LIMIT_REQUESTS_PER_MINUTE: number;

  TLS_ENABLED: boolean;
  TLS_CERT_PATH: string;
  TLS_KEY_PATH: string;

  // TCP proxy / WebSocket upgrade endpoints.
  TCP_ALLOW_PRIVATE_IPS: boolean;
  TCP_ALLOWED_HOSTS: string[];
  TCP_ALLOWED_PORTS: number[];
  TCP_BLOCKED_CLIENT_IPS: string[];
  TCP_MUX_MAX_STREAMS: number;
  TCP_MUX_MAX_STREAM_BUFFER_BYTES: number;
  TCP_MUX_MAX_FRAME_PAYLOAD_BYTES: number;

  // Session-scoped TCP proxy limits.
  TCP_PROXY_MAX_CONNECTIONS: number;
  TCP_PROXY_MAX_CONNECTIONS_PER_IP: number;
  TCP_PROXY_MAX_MESSAGE_BYTES: number;
  TCP_PROXY_CONNECT_TIMEOUT_MS: number;
  TCP_PROXY_IDLE_TIMEOUT_MS: number;

  // DNS-over-HTTPS (RFC8484)
  DNS_UPSTREAMS: string[];
  DNS_UPSTREAM_TIMEOUT_MS: number;

  DNS_CACHE_MAX_ENTRIES: number;
  DNS_CACHE_MAX_TTL_SECONDS: number;
  DNS_CACHE_NEGATIVE_TTL_SECONDS: number;

  DNS_MAX_QUERY_BYTES: number;
  DNS_MAX_RESPONSE_BYTES: number;

  DNS_ALLOW_ANY: boolean;
  DNS_ALLOW_PRIVATE_PTR: boolean;

  DNS_QPS_PER_IP: number;
  DNS_BURST_PER_IP: number;

  // Optional UDP relay (WebRTC + WebSocket UDP fallback) integration.
  UDP_RELAY_BASE_URL: string;
  UDP_RELAY_AUTH_MODE: UdpRelayAuthMode;
  UDP_RELAY_API_KEY: string;
  UDP_RELAY_JWT_SECRET: string;
  UDP_RELAY_TOKEN_TTL_SECONDS: number;
  UDP_RELAY_AUDIENCE: string;
  UDP_RELAY_ISSUER: string;

  /**
   * Optional L2 tunnel framing payload limits surfaced via `POST /session`.
   *
   * These should match the configured `aero-l2-proxy` limits (e.g.
   * `AERO_L2_MAX_FRAME_PAYLOAD`, `AERO_L2_MAX_CONTROL_PAYLOAD`) so browser clients
   * can cap their outbound frames accordingly.
   */
  L2_MAX_FRAME_PAYLOAD_BYTES?: number;
  L2_MAX_CONTROL_PAYLOAD_BYTES?: number;
}>;

type Env = Record<string, string | undefined>;

function formatForError(value: string, maxLen = 128): string {
  if (maxLen <= 0) return `(${value.length} chars)`;
  if (value.length <= maxLen) return value;
  return `${value.slice(0, maxLen)}…(${value.length} chars)`;
}

const MAX_ENV_INT_LEN = 64;
const MAX_ENV_ERROR_MESSAGE_BYTES = 256;
const DEFAULT_LIST_LIMITS = { maxLen: 64 * 1024, maxItems: 1024 } as const;

function splitCommaList(value: string, envName: string, opts?: { maxLen?: number; maxItems?: number }): string[] {
  try {
    return splitCommaSeparatedList(value, { maxLen: opts?.maxLen, maxItems: opts?.maxItems });
  } catch (err) {
    const msg = formatOneLineUtf8(err instanceof Error ? err.message : err, MAX_ENV_ERROR_MESSAGE_BYTES) || 'Error';
    throw new Error(`Invalid ${envName}: ${msg}`);
  }
}

function splitCommaListNumbers(value: string, envName: string): number[] {
  if (value.trim() === '') return [];
  return splitCommaList(value, envName, DEFAULT_LIST_LIMITS).map((raw) => {
    if (raw.length > MAX_ENV_INT_LEN) {
      throw new Error(`Invalid ${envName} number (too long)`);
    }
    const n = Number(raw);
    if (!Number.isInteger(n)) {
      throw new Error(`Invalid ${envName} number: ${formatForError(raw)}`);
    }
    return n;
  });
}
function assertReadableFile(filePath: string, envName: string): void {
  try {
    const stat = fs.statSync(filePath);
    if (!stat.isFile()) {
      throw new Error(`${envName} must point to a file: ${formatForError(filePath)}`);
    }
  } catch {
    throw new Error(`${envName} does not exist: ${formatForError(filePath)}`);
  }
}

const envSchema = z.object({
  HOST: z.string().min(1).default('0.0.0.0'),
  PORT: z.coerce.number().int().min(1).max(65535).default(8080),
  LOG_LEVEL: z.enum(logLevels).default('info'),
  ALLOWED_ORIGINS: z.string().optional().default(''),
  PUBLIC_BASE_URL: z.string().optional().default(''),
  SHUTDOWN_GRACE_MS: z.coerce.number().int().min(0).default(10_000),
  CROSS_ORIGIN_ISOLATION: z.string().optional().default('0'),
  TRUST_PROXY: z.enum(['0', '1']).optional().default('0'),

  SESSION_SECRET: z.string().optional().default(''),
  SESSION_TTL_SECONDS: z.coerce.number().int().min(1).default(60 * 60 * 24),
  SESSION_COOKIE_SAMESITE: z.enum(['Lax', 'Strict', 'None']).optional().default('Lax'),

  RATE_LIMIT_REQUESTS_PER_MINUTE: z.coerce.number().int().min(0).default(0),

  TLS_ENABLED: z.enum(['0', '1']).optional().default('0'),
  TLS_CERT_PATH: z.string().optional().default(''),
  TLS_KEY_PATH: z.string().optional().default(''),
  TCP_ALLOW_PRIVATE_IPS: z.enum(['0', '1']).optional().default('0'),
  TCP_ALLOWED_HOSTS: z.string().optional().default(''),
  TCP_ALLOWED_PORTS: z.string().optional().default(''),
  TCP_BLOCKED_CLIENT_IPS: z.string().optional().default(''),
  TCP_MUX_MAX_STREAMS: z.coerce.number().int().min(1).default(1024),
  TCP_MUX_MAX_STREAM_BUFFER_BYTES: z.coerce.number().int().min(0).default(1024 * 1024),
  TCP_MUX_MAX_FRAME_PAYLOAD_BYTES: z.coerce.number().int().min(0).default(16 * 1024 * 1024),

  TCP_PROXY_MAX_CONNECTIONS: z.coerce.number().int().min(1).default(64),
  TCP_PROXY_MAX_CONNECTIONS_PER_IP: z.coerce.number().int().min(0).default(0),
  TCP_PROXY_MAX_MESSAGE_BYTES: z.coerce.number().int().min(1).default(1024 * 1024),
  TCP_PROXY_CONNECT_TIMEOUT_MS: z.coerce.number().int().min(1).default(10_000),
  TCP_PROXY_IDLE_TIMEOUT_MS: z.coerce.number().int().min(1).default(300_000),

  DNS_UPSTREAMS: z.string().optional().default('1.1.1.1:53,8.8.8.8:53'),
  DNS_UPSTREAM_TIMEOUT_MS: z.coerce.number().int().min(1).default(2000),

  DNS_CACHE_MAX_ENTRIES: z.coerce.number().int().min(0).default(10_000),
  DNS_CACHE_MAX_TTL_SECONDS: z.coerce.number().int().min(0).default(300),
  DNS_CACHE_NEGATIVE_TTL_SECONDS: z.coerce.number().int().min(0).default(60),

  DNS_MAX_QUERY_BYTES: z.coerce.number().int().min(0).default(4096),
  DNS_MAX_RESPONSE_BYTES: z.coerce.number().int().min(0).default(4096),

  DNS_ALLOW_ANY: z.string().optional().default('0'),
  DNS_ALLOW_PRIVATE_PTR: z.string().optional().default('0'),

  DNS_QPS_PER_IP: z.coerce.number().min(0).default(10),
  DNS_BURST_PER_IP: z.coerce.number().min(0).default(20),

  UDP_RELAY_BASE_URL: z.string().optional().default(''),
  UDP_RELAY_AUTH_MODE: z.enum(udpRelayAuthModes).optional().default('none'),
  UDP_RELAY_API_KEY: z.string().optional().default(''),
  UDP_RELAY_JWT_SECRET: z.string().optional().default(''),
  UDP_RELAY_TOKEN_TTL_SECONDS: z.coerce.number().int().min(1).default(300),
  UDP_RELAY_AUDIENCE: z.string().optional().default(''),
  UDP_RELAY_ISSUER: z.string().optional().default(''),
});

export function loadConfig(env: Env = process.env): Config {
  const parsed = envSchema.safeParse(env);
  if (!parsed.success) {
    throw new Error(`Invalid configuration:\n${parsed.error.message}`);
  }

  const raw = parsed.data;
  const tlsEnabled = raw.TLS_ENABLED === '1';
  const trustProxy = raw.TRUST_PROXY === '1';
  const tlsCertPath = raw.TLS_CERT_PATH.trim();
  const tlsKeyPath = raw.TLS_KEY_PATH.trim();
  const sessionSecret = (env.AERO_GATEWAY_SESSION_SECRET ?? raw.SESSION_SECRET).trim();

  if (tlsEnabled) {
    if (!tlsCertPath) {
      throw new Error('TLS_CERT_PATH is required when TLS_ENABLED=1');
    }
    if (!tlsKeyPath) {
      throw new Error('TLS_KEY_PATH is required when TLS_ENABLED=1');
    }

    assertReadableFile(tlsCertPath, 'TLS_CERT_PATH');
    assertReadableFile(tlsKeyPath, 'TLS_KEY_PATH');
  }

  const tcpAllowedPorts = splitCommaListNumbers(raw.TCP_ALLOWED_PORTS, 'TCP_ALLOWED_PORTS');
  for (const port of tcpAllowedPorts) {
    if (port < 1 || port > 65535) {
      throw new Error(`Invalid TCP_ALLOWED_PORTS entry: ${port}`);
    }
  }

  const defaultScheme = tlsEnabled ? 'https' : 'http';
  const publicBaseUrl =
    raw.PUBLIC_BASE_URL.length > 0 ? raw.PUBLIC_BASE_URL : `${defaultScheme}://localhost:${raw.PORT}`;

  let publicBaseUrlParsed: URL;
  try {
    publicBaseUrlParsed = new URL(publicBaseUrl);
  } catch {
    throw new Error("Invalid PUBLIC_BASE_URL (expected an absolute http(s) URL)");
  }

  const allowedOrigins = splitCommaList(raw.ALLOWED_ORIGINS, 'ALLOWED_ORIGINS', { maxLen: 64 * 1024, maxItems: 1024 }).map(
    normalizeAllowedOriginString,
  );
  const allowedOriginsWithDefault =
    allowedOrigins.length > 0 ? allowedOrigins : [normalizeAllowedOriginString(publicBaseUrlParsed.origin)];

  const udpRelayBaseUrlRaw = raw.UDP_RELAY_BASE_URL.trim();
  let udpRelayBaseUrl = '';
  if (udpRelayBaseUrlRaw.length > 0) {
    let udpRelayParsed: URL;
    try {
      udpRelayParsed = new URL(udpRelayBaseUrlRaw);
    } catch {
      throw new Error("Invalid UDP_RELAY_BASE_URL (expected http(s):// or ws(s):// URL)");
    }

    if (!['http:', 'https:', 'ws:', 'wss:'].includes(udpRelayParsed.protocol)) {
      throw new Error(
        `Invalid UDP_RELAY_BASE_URL protocol (got ${formatForError(udpRelayParsed.protocol)})`,
      );
    }

    udpRelayBaseUrl = udpRelayParsed.toString().replace(/\/$/, '');
  }

  const udpRelayApiKey = raw.UDP_RELAY_API_KEY.trim();
  const udpRelayJwtSecret = raw.UDP_RELAY_JWT_SECRET.trim();
  const udpRelayAudience = raw.UDP_RELAY_AUDIENCE.trim();
  const udpRelayIssuer = raw.UDP_RELAY_ISSUER.trim();

  if (udpRelayBaseUrl) {
    if (raw.UDP_RELAY_AUTH_MODE === 'api_key' && !udpRelayApiKey) {
      throw new Error('UDP_RELAY_API_KEY is required when UDP_RELAY_AUTH_MODE=api_key');
    }
    if (raw.UDP_RELAY_AUTH_MODE === 'jwt' && !udpRelayJwtSecret) {
      throw new Error('UDP_RELAY_JWT_SECRET is required when UDP_RELAY_AUTH_MODE=jwt');
    }
  }

  function parseOptionalPositiveInt(name: string, value: string | undefined): number | null {
    const trimmed = value?.trim();
    if (!trimmed) return null;
    if (trimmed.length > MAX_ENV_INT_LEN) {
      throw new Error(`${name} value too long`);
    }
    const parsed = Number(trimmed);
    if (!Number.isInteger(parsed) || parsed < 0) {
      throw new Error(`${name} must be a positive integer (or 0 to unset), got ${formatForError(trimmed)}`);
    }
    // Treat 0 as "unset" so deployments can pass through empty/placeholder values without
    // accidentally disabling framing.
    if (parsed === 0) return null;
    return parsed;
  }

  // L2 tunnel payload sizes are owned by `crates/aero-l2-proxy` but are surfaced
  // via the gateway's session bootstrap response for client-side bounds checks.
  //
  // Support the proxy's canonical env var names as aliases so deployments can
  // share configuration across services.
  const l2MaxFramePayloadBytes =
    parseOptionalPositiveInt('AERO_L2_MAX_FRAME_PAYLOAD', env.AERO_L2_MAX_FRAME_PAYLOAD) ??
    parseOptionalPositiveInt('AERO_L2_MAX_FRAME_SIZE', env.AERO_L2_MAX_FRAME_SIZE) ??
    L2_TUNNEL_DEFAULT_MAX_FRAME_PAYLOAD_BYTES;
  const l2MaxControlPayloadBytes =
    parseOptionalPositiveInt('AERO_L2_MAX_CONTROL_PAYLOAD', env.AERO_L2_MAX_CONTROL_PAYLOAD) ??
    L2_TUNNEL_DEFAULT_MAX_CONTROL_PAYLOAD_BYTES;

  return {
    HOST: raw.HOST,
    PORT: raw.PORT,
    LOG_LEVEL: raw.LOG_LEVEL,
    ALLOWED_ORIGINS: allowedOriginsWithDefault,
    PUBLIC_BASE_URL: publicBaseUrlParsed.toString().replace(/\/$/, ''),
    SHUTDOWN_GRACE_MS: raw.SHUTDOWN_GRACE_MS,
    CROSS_ORIGIN_ISOLATION: raw.CROSS_ORIGIN_ISOLATION === '1',
    TRUST_PROXY: trustProxy,

    SESSION_SECRET: sessionSecret,
    SESSION_TTL_SECONDS: raw.SESSION_TTL_SECONDS,
    SESSION_COOKIE_SAMESITE: raw.SESSION_COOKIE_SAMESITE,

    RATE_LIMIT_REQUESTS_PER_MINUTE: raw.RATE_LIMIT_REQUESTS_PER_MINUTE,

    TLS_ENABLED: tlsEnabled,
    TLS_CERT_PATH: tlsCertPath,
    TLS_KEY_PATH: tlsKeyPath,
    TCP_ALLOW_PRIVATE_IPS: raw.TCP_ALLOW_PRIVATE_IPS === '1',
    TCP_ALLOWED_HOSTS: splitCommaList(raw.TCP_ALLOWED_HOSTS, 'TCP_ALLOWED_HOSTS', DEFAULT_LIST_LIMITS),
    TCP_ALLOWED_PORTS: tcpAllowedPorts,
    TCP_BLOCKED_CLIENT_IPS: splitCommaList(raw.TCP_BLOCKED_CLIENT_IPS, 'TCP_BLOCKED_CLIENT_IPS', DEFAULT_LIST_LIMITS),
    TCP_MUX_MAX_STREAMS: raw.TCP_MUX_MAX_STREAMS,
    TCP_MUX_MAX_STREAM_BUFFER_BYTES: raw.TCP_MUX_MAX_STREAM_BUFFER_BYTES,
    TCP_MUX_MAX_FRAME_PAYLOAD_BYTES: raw.TCP_MUX_MAX_FRAME_PAYLOAD_BYTES,

    TCP_PROXY_MAX_CONNECTIONS: raw.TCP_PROXY_MAX_CONNECTIONS,
    TCP_PROXY_MAX_CONNECTIONS_PER_IP: raw.TCP_PROXY_MAX_CONNECTIONS_PER_IP,
    TCP_PROXY_MAX_MESSAGE_BYTES: raw.TCP_PROXY_MAX_MESSAGE_BYTES,
    TCP_PROXY_CONNECT_TIMEOUT_MS: raw.TCP_PROXY_CONNECT_TIMEOUT_MS,
    TCP_PROXY_IDLE_TIMEOUT_MS: raw.TCP_PROXY_IDLE_TIMEOUT_MS,

    DNS_UPSTREAMS: splitCommaList(raw.DNS_UPSTREAMS, 'DNS_UPSTREAMS', DEFAULT_LIST_LIMITS),
    DNS_UPSTREAM_TIMEOUT_MS: raw.DNS_UPSTREAM_TIMEOUT_MS,

    DNS_CACHE_MAX_ENTRIES: raw.DNS_CACHE_MAX_ENTRIES,
    DNS_CACHE_MAX_TTL_SECONDS: raw.DNS_CACHE_MAX_TTL_SECONDS,
    DNS_CACHE_NEGATIVE_TTL_SECONDS: raw.DNS_CACHE_NEGATIVE_TTL_SECONDS,

    DNS_MAX_QUERY_BYTES: raw.DNS_MAX_QUERY_BYTES,
    DNS_MAX_RESPONSE_BYTES: raw.DNS_MAX_RESPONSE_BYTES,

    DNS_ALLOW_ANY: raw.DNS_ALLOW_ANY === '1',
    DNS_ALLOW_PRIVATE_PTR: raw.DNS_ALLOW_PRIVATE_PTR === '1',

    DNS_QPS_PER_IP: raw.DNS_QPS_PER_IP,
    DNS_BURST_PER_IP: raw.DNS_BURST_PER_IP,

    UDP_RELAY_BASE_URL: udpRelayBaseUrl,
    UDP_RELAY_AUTH_MODE: raw.UDP_RELAY_AUTH_MODE,
    UDP_RELAY_API_KEY: udpRelayApiKey,
    UDP_RELAY_JWT_SECRET: udpRelayJwtSecret,
    UDP_RELAY_TOKEN_TTL_SECONDS: raw.UDP_RELAY_TOKEN_TTL_SECONDS,
    UDP_RELAY_AUDIENCE: udpRelayAudience,
    UDP_RELAY_ISSUER: udpRelayIssuer,

    L2_MAX_FRAME_PAYLOAD_BYTES: l2MaxFramePayloadBytes,
    L2_MAX_CONTROL_PAYLOAD_BYTES: l2MaxControlPayloadBytes,
  };
}
