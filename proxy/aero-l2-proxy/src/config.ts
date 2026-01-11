export interface L2ProxyConfig {
  listenHost: string;
  listenPort: number;
  open: boolean;
  allowedOrigins: string[];
  token: string | null;
  maxConnections: number;
  maxBytesPerConnection: number;
  maxFramesPerSecond: number;
  wsMaxPayloadBytes: number;
}

function readEnvInt(name: string, fallback: number): number {
  const raw = process.env[name];
  if (raw === undefined || raw === "") return fallback;
  const parsed = Number(raw);
  if (!Number.isFinite(parsed) || !Number.isInteger(parsed) || parsed < 0) {
    throw new Error(`Invalid ${name}=${raw}`);
  }
  return parsed;
}

function parseAllowedOrigins(raw: string | undefined): string[] {
  if (!raw) return [];
  return raw
    .split(",")
    .map((s) => s.trim())
    .filter((s) => s.length > 0);
}

export function loadConfigFromEnv(): L2ProxyConfig {
  const tokenRaw = process.env.AERO_L2_TOKEN;
  const token = tokenRaw && tokenRaw !== "" ? tokenRaw : null;

  return {
    listenHost: process.env.AERO_L2_LISTEN_HOST ?? "127.0.0.1",
    listenPort: readEnvInt("AERO_L2_PORT", 8082),

    open: process.env.AERO_L2_OPEN === "1",
    allowedOrigins: parseAllowedOrigins(process.env.AERO_L2_ALLOWED_ORIGINS),
    token,

    maxConnections: readEnvInt("AERO_L2_MAX_CONNECTIONS", 64),
    maxBytesPerConnection: readEnvInt("AERO_L2_MAX_BYTES_PER_CONNECTION", 0),
    maxFramesPerSecond: readEnvInt("AERO_L2_MAX_FRAMES_PER_SECOND", 0),
    wsMaxPayloadBytes: readEnvInt("AERO_L2_WS_MAX_PAYLOAD_BYTES", 1 * 1024 * 1024),
  };
}
