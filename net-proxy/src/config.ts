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
  tcpMuxMaxStreams: number;
  tcpMuxMaxStreamBufferedBytes: number;
  tcpMuxMaxFramePayloadBytes: number;
  udpRelayMaxPayloadBytes: number;
  udpRelayMaxBindingsPerConnection: number;
  udpRelayBindingIdleTimeoutMs: number;
  udpRelayPreferV2: boolean;
  udpRelayInboundFilterMode: "any" | "address_and_port";
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

function readEnvBool(name: string, fallback: boolean): boolean {
  const raw = process.env[name];
  if (raw === undefined || raw === "") return fallback;
  const normalized = raw.trim().toLowerCase();
  if (normalized === "1" || normalized === "true") return true;
  if (normalized === "0" || normalized === "false") return false;
  throw new Error(`Invalid ${name}=${raw} (expected 0/1/true/false)`);
}

export function loadConfigFromEnv(): ProxyConfig {
  const tcpMuxMaxStreams = readEnvInt("AERO_PROXY_TCP_MUX_MAX_STREAMS", 1024);
  if (tcpMuxMaxStreams < 1) {
    throw new Error(`Invalid AERO_PROXY_TCP_MUX_MAX_STREAMS=${tcpMuxMaxStreams} (must be >= 1)`);
  }

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
    listenPort: readEnvInt("AERO_PROXY_PORT", 8081),
    open: process.env.AERO_PROXY_OPEN === "1",
    allow: process.env.AERO_PROXY_ALLOW ?? "",
    connectTimeoutMs: readEnvInt("AERO_PROXY_CONNECT_TIMEOUT_MS", 10_000),
    dnsTimeoutMs: readEnvInt("AERO_PROXY_DNS_TIMEOUT_MS", 5_000),
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
