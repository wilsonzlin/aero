import { splitCommaSeparatedList } from "./csv";
import { formatOneLineError } from "./text";

export interface ProxyConfig {
  listenHost: string;
  listenPort: number;
  open: boolean;
  allow: string;
  connectTimeoutMs: number;
  dnsTimeoutMs: number;
  dohMaxQueryBytes: number;
  dohMaxQnameLength: number;
  dohAnswerTtlSeconds: number;
  dohMaxAnswerTtlSeconds: number;
  dohMaxAnswers: number;
  dohCorsAllowOrigins: string[];
  wsMaxPayloadBytes: number;
  wsStreamHighWaterMarkBytes: number;
  udpWsBufferedAmountLimitBytes: number;
  tcpMuxMaxStreams: number;
  tcpMuxMaxStreamBufferedBytes: number;
  tcpMuxMaxFramePayloadBytes: number;
  udpRelayMaxPayloadBytes: number;
  udpRelayMaxBindingsPerConnection: number;
  udpRelayBindingIdleTimeoutMs: number;
  udpRelayPreferV2: boolean;
  udpRelayInboundFilterMode: "any" | "address_and_port";
}

const MAX_ENV_INT_LEN = 64;

function formatForError(value: string, maxLen = 128): string {
  if (maxLen <= 0) return `(${value.length} chars)`;
  if (value.length <= maxLen) return value;
  return `${value.slice(0, maxLen)}…(${value.length} chars)`;
}

function readEnvInt(name: string, fallback: number, opts?: { min?: number; max?: number }): number {
  const raw = process.env[name];
  if (raw === undefined) return fallback;
  const trimmed = raw.trim();
  if (trimmed === "") return fallback;
  if (trimmed.length > MAX_ENV_INT_LEN) {
    throw new Error(`Invalid ${name} (value too long)`);
  }
  if (!/^[+-]?\d+$/.test(trimmed)) {
    throw new Error(`Invalid ${name}`);
  }

  const parsed = Number.parseInt(trimmed, 10);
  if (!Number.isSafeInteger(parsed)) {
    throw new Error(`Invalid ${name}`);
  }

  const min = opts?.min ?? 0;
  const max = opts?.max ?? Number.MAX_SAFE_INTEGER;
  if (parsed < min || parsed > max) {
    throw new Error(`Invalid ${name}`);
  }
  return parsed;
}

function readEnvBool(name: string, fallback: boolean): boolean {
  const raw = process.env[name];
  if (raw === undefined || raw === "") return fallback;
  if (raw.length > MAX_ENV_INT_LEN) {
    throw new Error(`Invalid ${name} (value too long)`);
  }
  const normalized = raw.trim().toLowerCase();
  if (normalized === "1" || normalized === "true") return true;
  if (normalized === "0" || normalized === "false") return false;
  throw new Error(`Invalid ${name} (expected 0/1/true/false)`);
}

function readEnvOriginAllowlist(name: string): string[] {
  const raw = process.env[name];
  if (raw === undefined || raw.trim() === "") return [];

  let parts: string[];
  try {
    parts = splitCommaSeparatedList(raw, { maxLen: 64 * 1024, maxItems: 1024 });
  } catch (err) {
    const msg = formatOneLineError(err, 256, "invalid");
    throw new Error(`Invalid ${name}: ${msg}`);
  }

  const out: string[] = [];
  for (const part of parts) {
    if (part === "*") {
      out.push("*");
      continue;
    }
    // Browsers use the literal string "null" for opaque origins such as file:// URLs
    // and sandboxed iframes. Allow explicitly opting into that case for local dev.
    if (part.toLowerCase() === "null") {
      out.push("null");
      continue;
    }
    try {
      const url = new URL(part);
      out.push(url.origin);
    } catch {
      throw new Error(`Invalid ${name} origin: ${formatForError(part)}`);
    }
  }

  // Deduplicate while preserving order.
  const seen = new Set<string>();
  const deduped: string[] = [];
  for (const value of out) {
    if (seen.has(value)) continue;
    seen.add(value);
    deduped.push(value);
  }
  return deduped;
}

export function loadConfigFromEnv(): ProxyConfig {
  const tcpMuxMaxStreams = readEnvInt("AERO_PROXY_TCP_MUX_MAX_STREAMS", 1024);
  if (tcpMuxMaxStreams < 1) {
    throw new Error(`Invalid AERO_PROXY_TCP_MUX_MAX_STREAMS=${tcpMuxMaxStreams} (must be >= 1)`);
  }

  const dohMaxQueryBytes = readEnvInt("AERO_PROXY_DOH_MAX_QUERY_BYTES", 512);
  if (dohMaxQueryBytes < 1) {
    throw new Error(`Invalid AERO_PROXY_DOH_MAX_QUERY_BYTES=${dohMaxQueryBytes} (must be >= 1)`);
  }

  const dohMaxQnameLength = readEnvInt("AERO_PROXY_DOH_MAX_QNAME_LENGTH", 253);
  if (dohMaxQnameLength < 1 || dohMaxQnameLength > 253) {
    throw new Error(`Invalid AERO_PROXY_DOH_MAX_QNAME_LENGTH=${dohMaxQnameLength} (must be 1..253)`);
  }

  const dohAnswerTtlSeconds = readEnvInt("AERO_PROXY_DOH_ANSWER_TTL_SECONDS", 60);
  const dohMaxAnswerTtlSeconds = readEnvInt("AERO_PROXY_DOH_MAX_ANSWER_TTL_SECONDS", 300);
  const dohMaxAnswers = readEnvInt("AERO_PROXY_DOH_MAX_ANSWERS", 16);
  const dohCorsAllowOrigins = readEnvOriginAllowlist("AERO_PROXY_DOH_CORS_ALLOW_ORIGINS");

  const udpRelayInboundFilterModeRaw = (process.env.AERO_PROXY_UDP_RELAY_INBOUND_FILTER_MODE ?? "address_and_port")
    .trim()
    .toLowerCase();
  let udpRelayInboundFilterMode: ProxyConfig["udpRelayInboundFilterMode"];
  if (udpRelayInboundFilterModeRaw === "any" || udpRelayInboundFilterModeRaw === "full_cone") {
    udpRelayInboundFilterMode = "any";
  } else if (
    udpRelayInboundFilterModeRaw === "address_and_port" ||
    udpRelayInboundFilterModeRaw === "addrport" ||
    udpRelayInboundFilterModeRaw === "address+port"
  ) {
    udpRelayInboundFilterMode = "address_and_port";
  } else {
    throw new Error(
      `Invalid AERO_PROXY_UDP_RELAY_INBOUND_FILTER_MODE=${udpRelayInboundFilterModeRaw} (expected any or address_and_port)`
    );
  }

  return {
    listenHost: process.env.AERO_PROXY_LISTEN_HOST ?? "127.0.0.1",
    listenPort: readEnvInt("AERO_PROXY_PORT", 8081, { min: 0, max: 65535 }),
    open: process.env.AERO_PROXY_OPEN === "1",
    allow: process.env.AERO_PROXY_ALLOW ?? "",
    connectTimeoutMs: readEnvInt("AERO_PROXY_CONNECT_TIMEOUT_MS", 10_000),
    dnsTimeoutMs: readEnvInt("AERO_PROXY_DNS_TIMEOUT_MS", 5_000),
    dohMaxQueryBytes,
    dohMaxQnameLength,
    dohAnswerTtlSeconds,
    dohMaxAnswerTtlSeconds,
    dohMaxAnswers,
    dohCorsAllowOrigins,
    wsMaxPayloadBytes: readEnvInt("AERO_PROXY_WS_MAX_PAYLOAD_BYTES", 1 * 1024 * 1024),
    wsStreamHighWaterMarkBytes: readEnvInt("AERO_PROXY_WS_STREAM_HWM_BYTES", 64 * 1024),
    udpWsBufferedAmountLimitBytes: readEnvInt("AERO_PROXY_UDP_WS_BUFFER_LIMIT_BYTES", 1 * 1024 * 1024),
    tcpMuxMaxStreams,
    tcpMuxMaxStreamBufferedBytes: readEnvInt("AERO_PROXY_TCP_MUX_MAX_STREAM_BUFFER_BYTES", 1024 * 1024),
    tcpMuxMaxFramePayloadBytes: readEnvInt("AERO_PROXY_TCP_MUX_MAX_FRAME_PAYLOAD_BYTES", 16 * 1024 * 1024),
    udpRelayMaxPayloadBytes: readEnvInt("AERO_PROXY_UDP_RELAY_MAX_PAYLOAD_BYTES", 1200),
    udpRelayMaxBindingsPerConnection: readEnvInt("AERO_PROXY_UDP_RELAY_MAX_BINDINGS", 128),
    udpRelayBindingIdleTimeoutMs: readEnvInt("AERO_PROXY_UDP_RELAY_BINDING_IDLE_TIMEOUT_MS", 60_000),
    udpRelayPreferV2: readEnvBool("AERO_PROXY_UDP_RELAY_PREFER_V2", false),
    udpRelayInboundFilterMode
  };
}
