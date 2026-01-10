import fs from 'node:fs';
import { z } from 'zod';

const logLevels = ['fatal', 'error', 'warn', 'info', 'debug', 'trace', 'silent'] as const;

export type LogLevel = (typeof logLevels)[number];

export type Config = Readonly<{
  HOST: string;
  PORT: number;
  LOG_LEVEL: LogLevel;
  ALLOWED_ORIGINS: string[];
  PUBLIC_BASE_URL: string;
  SHUTDOWN_GRACE_MS: number;
  CROSS_ORIGIN_ISOLATION: boolean;
  TRUST_PROXY: boolean;

  RATE_LIMIT_REQUESTS_PER_MINUTE: number;

  TLS_ENABLED: boolean;
  TLS_CERT_PATH: string;
  TLS_KEY_PATH: string;

  // Placeholders for upcoming features (not implemented in this skeleton).
  TCP_PROXY_MAX_CONNECTIONS: number;
  TCP_PROXY_MAX_CONNECTIONS_PER_IP: number;

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
}>;

type Env = Record<string, string | undefined>;

function splitCommaList(value: string): string[] {
  return value
    .split(',')
    .map((entry) => entry.trim())
    .filter((entry) => entry.length > 0);
}

function normalizeOrigin(maybeOrigin: string): string {
  const trimmed = maybeOrigin.trim();
  if (trimmed === '*' || trimmed === 'null') return trimmed;

  let url: URL;
  try {
    url = new URL(trimmed);
  } catch {
    throw new Error(`Invalid origin "${trimmed}". Expected a full origin like "https://example.com".`);
  }

  return url.origin;
}

function assertReadableFile(filePath: string, envName: string): void {
  try {
    const stat = fs.statSync(filePath);
    if (!stat.isFile()) {
      throw new Error(`${envName} must point to a file: ${filePath}`);
    }
  } catch {
    throw new Error(`${envName} does not exist: ${filePath}`);
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

  RATE_LIMIT_REQUESTS_PER_MINUTE: z.coerce.number().int().min(0).default(0),

  TLS_ENABLED: z.enum(['0', '1']).optional().default('0'),
  TLS_CERT_PATH: z.string().optional().default(''),
  TLS_KEY_PATH: z.string().optional().default(''),

  // Placeholders (unused today).
  TCP_PROXY_MAX_CONNECTIONS: z.coerce.number().int().min(0).default(0),
  TCP_PROXY_MAX_CONNECTIONS_PER_IP: z.coerce.number().int().min(0).default(0),

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

  const defaultScheme = tlsEnabled ? 'https' : 'http';
  const publicBaseUrl =
    raw.PUBLIC_BASE_URL.length > 0 ? raw.PUBLIC_BASE_URL : `${defaultScheme}://localhost:${raw.PORT}`;

  let publicBaseUrlParsed: URL;
  try {
    publicBaseUrlParsed = new URL(publicBaseUrl);
  } catch {
    throw new Error(`Invalid PUBLIC_BASE_URL "${publicBaseUrl}". Expected a URL like "https://example.com".`);
  }

  const allowedOrigins = splitCommaList(raw.ALLOWED_ORIGINS).map(normalizeOrigin);
  const allowedOriginsWithDefault =
    allowedOrigins.length > 0 ? allowedOrigins : [normalizeOrigin(publicBaseUrlParsed.origin)];

  return {
    HOST: raw.HOST,
    PORT: raw.PORT,
    LOG_LEVEL: raw.LOG_LEVEL,
    ALLOWED_ORIGINS: allowedOriginsWithDefault,
    PUBLIC_BASE_URL: publicBaseUrlParsed.toString().replace(/\/$/, ''),
    SHUTDOWN_GRACE_MS: raw.SHUTDOWN_GRACE_MS,
    CROSS_ORIGIN_ISOLATION: raw.CROSS_ORIGIN_ISOLATION === '1',
    TRUST_PROXY: trustProxy,

    RATE_LIMIT_REQUESTS_PER_MINUTE: raw.RATE_LIMIT_REQUESTS_PER_MINUTE,

    TLS_ENABLED: tlsEnabled,
    TLS_CERT_PATH: tlsCertPath,
    TLS_KEY_PATH: tlsKeyPath,

    TCP_PROXY_MAX_CONNECTIONS: raw.TCP_PROXY_MAX_CONNECTIONS,
    TCP_PROXY_MAX_CONNECTIONS_PER_IP: raw.TCP_PROXY_MAX_CONNECTIONS_PER_IP,

    DNS_UPSTREAMS: splitCommaList(raw.DNS_UPSTREAMS),
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
  };
}
