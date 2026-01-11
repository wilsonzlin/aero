import { WebSocketL2TunnelClient, type L2TunnelClientOptions, type L2TunnelSink } from "./l2Tunnel";
import { connectL2Relay } from "./l2RelaySignalingClient";
import type { RelaySignalingMode } from "./webrtcRelaySignalingClient";

export type L2TunnelTransportMode = "ws" | "webrtc";

export type ConnectL2TunnelOptions = Readonly<{
  /**
   * Underlying transport for the L2 tunnel.
   *
   * - `ws` (default): direct WebSocket to `/l2` on the gateway origin. Requires a
   *   prior `POST /session` to mint the `aero_session` cookie.
   * - `webrtc`: WebRTC DataChannel via the UDP relay returned by `POST /session`
   *   (`udpRelay` config).
   */
  mode?: L2TunnelTransportMode;

  /**
   * L2 tunnel event sink (`open`/`frame`/`pong`/`close`/`error`).
   */
  sink: L2TunnelSink;

  /**
   * Backpressure/keepalive options forwarded to the underlying tunnel client.
   */
  tunnelOptions?: L2TunnelClientOptions;

  /**
   * Advanced: signaling mode for the WebRTC relay.
   *
   * Defaults to the relay client's default (`ws-trickle`).
   */
  relaySignalingMode?: RelaySignalingMode;
}>;

export type ConnectedL2Tunnel = Readonly<{
  sendFrame(frame: Uint8Array): void;
  close(): void;
}>;

type GatewaySessionResponse = Readonly<{
  udpRelay?: Readonly<{
    baseUrl: string;
    token?: string;
  }>;
}>;

function buildSessionUrl(gatewayBaseUrl: string): string {
  const url = new URL(gatewayBaseUrl);

  // `fetch()` does not support ws(s) schemes. If the caller passed a WebSocket
  // URL (explicit `/l2`), map it back to the HTTP origin for session bootstrap.
  if (url.protocol === "ws:") url.protocol = "http:";
  if (url.protocol === "wss:") url.protocol = "https:";

  // Session bootstrap is an HTTP endpoint and does not accept query parameters.
  url.search = "";
  url.hash = "";

  // `gatewayBaseUrl` may be a base path (append `/l2` later) or an explicit L2
  // endpoint (`.../l2` or legacy `.../eth`). The session endpoint is a sibling.
  let path = url.pathname.replace(/\/$/, "");
  if (path.endsWith("/l2") || path.endsWith("/eth")) {
    path = path.replace(/\/(l2|eth)$/, "");
  }

  url.pathname = `${path.replace(/\/$/, "")}/session`;
  return url.toString();
}

async function bootstrapSession(gatewayBaseUrl: string): Promise<string> {
  const res = await fetch(buildSessionUrl(gatewayBaseUrl), {
    method: "POST",
    credentials: "include",
    headers: { "content-type": "application/json" },
    body: "{}",
  });

  const text = await res.text();
  if (!res.ok) {
    throw new Error(`failed to bootstrap gateway session (${res.status}): ${text}`);
  }
  return text;
}

function parseGatewaySessionResponse(text: string): GatewaySessionResponse {
  const trimmed = text.trim();
  if (trimmed.length === 0) return {};
  const json: unknown = JSON.parse(trimmed);
  if (typeof json !== "object" || json === null) return {};
  return json as GatewaySessionResponse;
}

/**
 * High-level connector for the Option C L2 tunnel.
 *
 * Always bootstraps a gateway session first (`POST /session`) so browsers have
 * the `aero_session` cookie needed by WebSocket `/l2` and, when configured, the
 * UDP relay metadata needed for WebRTC tunneling.
 */
export async function connectL2Tunnel(gatewayBaseUrl: string, opts: ConnectL2TunnelOptions): Promise<ConnectedL2Tunnel> {
  const mode = opts.mode ?? "ws";

  const sessionText = await bootstrapSession(gatewayBaseUrl);

  if (mode === "ws") {
    const l2 = new WebSocketL2TunnelClient(gatewayBaseUrl, opts.sink, opts.tunnelOptions);
    l2.connect();
    return {
      sendFrame: (frame) => l2.sendFrame(frame),
      close: () => l2.close(),
    };
  }

  if (mode === "webrtc") {
    let session: GatewaySessionResponse;
    try {
      session = parseGatewaySessionResponse(sessionText);
    } catch (err) {
      throw new Error(`invalid gateway session response JSON: ${(err as Error).message}`);
    }

    const relay = session.udpRelay;
    if (!relay) {
      throw new Error(
        "mode=webrtc requested but gateway session response did not include udpRelay; ensure the gateway is configured with UDP_RELAY_BASE_URL",
      );
    }

    const { l2, close } = await connectL2Relay({
      baseUrl: relay.baseUrl,
      authToken: relay.token,
      mode: opts.relaySignalingMode,
      sink: opts.sink,
      tunnelOptions: opts.tunnelOptions,
    });

    return {
      sendFrame: (frame) => l2.sendFrame(frame),
      close,
    };
  }

  // Exhaustiveness guard for future modes.
  throw new Error(`unsupported L2 tunnel mode: ${String(mode)}`);
}
