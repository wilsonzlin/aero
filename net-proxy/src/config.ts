export interface ProxyConfig {
  listenHost: string;
  listenPort: number;
  open: boolean;
  allow: string;
  connectTimeoutMs: number;
  dnsTimeoutMs: number;
  wsMaxPayloadBytes: number;
  wsStreamHighWaterMarkBytes: number;
  udpWsBufferedAmountLimitBytes: number;
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

export function loadConfigFromEnv(): ProxyConfig {
  return {
    listenHost: process.env.AERO_PROXY_LISTEN_HOST ?? "127.0.0.1",
    listenPort: readEnvInt("AERO_PROXY_PORT", 8081),
    open: process.env.AERO_PROXY_OPEN === "1",
    allow: process.env.AERO_PROXY_ALLOW ?? "",
    connectTimeoutMs: readEnvInt("AERO_PROXY_CONNECT_TIMEOUT_MS", 10_000),
    dnsTimeoutMs: readEnvInt("AERO_PROXY_DNS_TIMEOUT_MS", 5_000),
    wsMaxPayloadBytes: readEnvInt("AERO_PROXY_WS_MAX_PAYLOAD_BYTES", 1 * 1024 * 1024),
    wsStreamHighWaterMarkBytes: readEnvInt("AERO_PROXY_WS_STREAM_HWM_BYTES", 64 * 1024),
    udpWsBufferedAmountLimitBytes: readEnvInt("AERO_PROXY_UDP_WS_BUFFER_LIMIT_BYTES", 1 * 1024 * 1024)
  };
}

