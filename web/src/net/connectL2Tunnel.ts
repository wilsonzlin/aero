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

  /**
   * Automatically reconnect when the tunnel closes unexpectedly.
   *
   * Reconnect attempts use exponential backoff with jitter. Manual `close()` will
   * always stop reconnect attempts.
   *
   * Default: true
   */
  reconnect?: boolean;

  /**
   * Base delay (ms) for reconnect backoff.
   *
   * Default: 250ms
   */
  reconnectBaseDelayMs?: number;

  /**
   * Maximum delay (ms) for reconnect backoff.
   *
   * Default: 30_000ms
   */
  reconnectMaxDelayMs?: number;

  /**
   * Jitter fraction for reconnect backoff (0..1). The actual delay is randomized
   * within ±(delay * jitterFraction) to avoid synchronized reconnect storms.
   *
   * Default: 0.2
   */
  reconnectJitterFraction?: number;
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

function validateNonNegativeInt(name: string, value: number): void {
  if (!Number.isInteger(value) || value < 0) {
    throw new RangeError(`${name} must be a non-negative integer (got ${value})`);
  }
}

function validateFraction01(name: string, value: number): void {
  if (!Number.isFinite(value) || value < 0 || value > 1) {
    throw new RangeError(`${name} must be between 0 and 1 (got ${value})`);
  }
}

function computeBackoffDelayMs(attempt: number, baseDelayMs: number, maxDelayMs: number, jitterFraction: number): number {
  // attempt is 1-based.
  const unclamped = baseDelayMs * 2 ** Math.max(0, attempt - 1);
  const delay = Math.min(maxDelayMs, unclamped);
  const jitter = delay * jitterFraction;
  const randomized = delay + (Math.random() * 2 - 1) * jitter;
  return Math.max(0, Math.round(randomized));
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
  const reconnectEnabled = opts.reconnect ?? true;
  const reconnectBaseDelayMs = opts.reconnectBaseDelayMs ?? 250;
  const reconnectMaxDelayMs = opts.reconnectMaxDelayMs ?? 30_000;
  const reconnectJitterFraction = opts.reconnectJitterFraction ?? 0.2;

  validateNonNegativeInt("reconnectBaseDelayMs", reconnectBaseDelayMs);
  validateNonNegativeInt("reconnectMaxDelayMs", reconnectMaxDelayMs);
  validateFraction01("reconnectJitterFraction", reconnectJitterFraction);
  if (reconnectBaseDelayMs > reconnectMaxDelayMs) {
    throw new RangeError(
      `reconnectBaseDelayMs must be <= reconnectMaxDelayMs (${reconnectBaseDelayMs} > ${reconnectMaxDelayMs})`,
    );
  }

  const errorIntervalMs = opts.tunnelOptions?.errorIntervalMs ?? 1000;
  validateNonNegativeInt("errorIntervalMs", errorIntervalMs);

  let reconnectAttempts = 0;
  let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  let closed = false;
  let generation = 0;

  let currentSendFrame: ((frame: Uint8Array) => void) | null = null;
  let currentClose: (() => void) | null = null;

  let lastErrorEmitAt = 0;
  const emitErrorThrottled = (error: unknown) => {
    const now = Date.now();
    if (errorIntervalMs > 0 && now - lastErrorEmitAt < errorIntervalMs) return;
    lastErrorEmitAt = now;
    opts.sink({ type: "error", error });
  };

  const clearReconnectTimer = () => {
    if (reconnectTimer === null) return;
    clearTimeout(reconnectTimer);
    reconnectTimer = null;
  };

  const scheduleReconnect = () => {
    if (closed) return;
    if (!reconnectEnabled) return;
    if (reconnectTimer !== null) return;
    reconnectAttempts += 1;
    const delayMs = computeBackoffDelayMs(
      reconnectAttempts,
      reconnectBaseDelayMs,
      reconnectMaxDelayMs,
      reconnectJitterFraction,
    );
    reconnectTimer = setTimeout(async () => {
      reconnectTimer = null;
      try {
        await reconnectNow();
      } catch (err) {
        // reconnectNow should handle its own scheduling, but keep a safety net
        // here to avoid an unhandled rejection.
        emitErrorThrottled(err);
      }
    }, delayMs);
  };

  const makeSink = (gen: number): L2TunnelSink => {
    return (ev) => {
      // Ignore late events from superseded tunnels.
      if (gen !== generation) return;

      if (ev.type === "open") {
        reconnectAttempts = 0;
      } else if (ev.type === "close") {
        // Forward the close event first so callers can treat the tunnel as
        // disconnected immediately.
        opts.sink(ev);
        scheduleReconnect();
        return;
      }

      opts.sink(ev);
    };
  };

  const sessionText = await bootstrapSession(gatewayBaseUrl);

  const installTunnel = (sendFrame: (frame: Uint8Array) => void, close: () => void) => {
    currentSendFrame = sendFrame;
    currentClose = close;
  };

  const teardownTunnel = () => {
    const close = currentClose;
    currentSendFrame = null;
    currentClose = null;
    try {
      close?.();
    } catch {
      // Ignore.
    }
  };

  const reconnectNow = async () => {
    if (closed) return;
    teardownTunnel();

    const gen = generation + 1;
    generation = gen;

    if (mode === "ws") {
      try {
        await bootstrapSession(gatewayBaseUrl);
      } catch (err) {
        emitErrorThrottled(err);
        scheduleReconnect();
        return;
      }

      const l2 = new WebSocketL2TunnelClient(gatewayBaseUrl, makeSink(gen), opts.tunnelOptions);
      l2.connect();
      installTunnel((frame) => l2.sendFrame(frame), () => l2.close());
      return;
    }

    if (mode === "webrtc") {
      let nextSessionText: string;
      try {
        nextSessionText = await bootstrapSession(gatewayBaseUrl);
      } catch (err) {
        emitErrorThrottled(err);
        scheduleReconnect();
        return;
      }

      let session: GatewaySessionResponse;
      try {
        session = parseGatewaySessionResponse(nextSessionText);
      } catch (err) {
        emitErrorThrottled(new Error(`invalid gateway session response JSON: ${(err as Error).message}`));
        scheduleReconnect();
        return;
      }

      const relay = session.udpRelay;
      if (!relay) {
        emitErrorThrottled(
          new Error(
            "mode=webrtc requested but gateway session response did not include udpRelay; ensure the gateway is configured with UDP_RELAY_BASE_URL",
          ),
        );
        return;
      }

      try {
        const { l2, close } = await connectL2Relay({
          baseUrl: relay.baseUrl,
          authToken: relay.token,
          mode: opts.relaySignalingMode,
          sink: makeSink(gen),
          tunnelOptions: opts.tunnelOptions,
        });
        if (closed) {
          close();
          return;
        }
        installTunnel((frame) => l2.sendFrame(frame), close);
      } catch (err) {
        emitErrorThrottled(err);
        scheduleReconnect();
      }
      return;
    }
  };

  if (mode === "ws") {
    generation = 1;
    const l2 = new WebSocketL2TunnelClient(gatewayBaseUrl, makeSink(generation), opts.tunnelOptions);
    l2.connect();
    installTunnel((frame) => l2.sendFrame(frame), () => l2.close());
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

    generation = 1;
    const { l2, close } = await connectL2Relay({
      baseUrl: relay.baseUrl,
      authToken: relay.token,
      mode: opts.relaySignalingMode,
      sink: makeSink(generation),
      tunnelOptions: opts.tunnelOptions,
    });
    installTunnel((frame) => l2.sendFrame(frame), close);
  }

  const tunnel: ConnectedL2Tunnel = {
    sendFrame(frame: Uint8Array): void {
      if (!currentSendFrame) {
        emitErrorThrottled(new Error("L2 tunnel is not connected"));
        return;
      }
      currentSendFrame(frame);
    },
    close(): void {
      if (closed) return;
      closed = true;
      clearReconnectTimer();
      teardownTunnel();
    },
  };

  return tunnel;
}
