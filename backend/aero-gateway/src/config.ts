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

  // Placeholders for upcoming features (not implemented in this skeleton).
  TCP_PROXY_MAX_CONNECTIONS: number;
  TCP_PROXY_MAX_CONNECTIONS_PER_IP: number;
  DNS_UPSTREAMS: string[];
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
  } catch (err) {
    throw new Error(`Invalid origin "${trimmed}". Expected a full origin like "https://example.com".`);
  }

  return url.origin;
}

const envSchema = z.object({
  HOST: z.string().min(1).default('0.0.0.0'),
  PORT: z.coerce.number().int().min(1).max(65535).default(8080),
  LOG_LEVEL: z.enum(logLevels).default('info'),
  ALLOWED_ORIGINS: z.string().optional().default(''),
  PUBLIC_BASE_URL: z.string().optional().default(''),
  SHUTDOWN_GRACE_MS: z.coerce.number().int().min(0).default(10_000),
  CROSS_ORIGIN_ISOLATION: z.string().optional().default('0'),
  TRUST_PROXY: z.string().optional().default('0'),

  RATE_LIMIT_REQUESTS_PER_MINUTE: z.coerce.number().int().min(0).default(0),

  // Placeholders (unused today).
  TCP_PROXY_MAX_CONNECTIONS: z.coerce.number().int().min(0).default(0),
  TCP_PROXY_MAX_CONNECTIONS_PER_IP: z.coerce.number().int().min(0).default(0),
  DNS_UPSTREAMS: z.string().optional().default(''),
});

export function loadConfig(env: Env = process.env): Config {
  const parsed = envSchema.safeParse(env);
  if (!parsed.success) {
    throw new Error(`Invalid configuration:\n${parsed.error.message}`);
  }

  const raw = parsed.data;
  const publicBaseUrl = raw.PUBLIC_BASE_URL.length > 0 ? raw.PUBLIC_BASE_URL : `http://localhost:${raw.PORT}`;

  let publicBaseUrlParsed: URL;
  try {
    publicBaseUrlParsed = new URL(publicBaseUrl);
  } catch (err) {
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
    TRUST_PROXY: raw.TRUST_PROXY === '1',

    RATE_LIMIT_REQUESTS_PER_MINUTE: raw.RATE_LIMIT_REQUESTS_PER_MINUTE,

    TCP_PROXY_MAX_CONNECTIONS: raw.TCP_PROXY_MAX_CONNECTIONS,
    TCP_PROXY_MAX_CONNECTIONS_PER_IP: raw.TCP_PROXY_MAX_CONNECTIONS_PER_IP,
    DNS_UPSTREAMS: splitCommaList(raw.DNS_UPSTREAMS),
  };
}
