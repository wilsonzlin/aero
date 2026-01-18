import { WebSocketL2TunnelClient, type L2TunnelClientOptions, type L2TunnelSink } from "./l2Tunnel";
import { connectL2Relay } from "./l2RelaySignalingClient";
import type { RelaySignalingMode } from "./webrtcRelaySignalingClient";
import { readTextResponseWithLimit } from "../storage/response_json";
import { formatOneLineError } from "../text";
import { unrefBestEffort } from "../unrefSafe";

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

  /**
   * Reconnect if the tunnel goes "silent" for this long (no inbound FRAME/PONG).
   *
   * This is primarily a defense against half-open connections where the browser
   * transport never fires `close`/`error` but the peer is no longer reachable.
   *
   * Default: `2 * keepaliveMaxMs` (or disabled when keepaliveMaxMs=0).
   */
  idleTimeoutMs?: number;
}>;

export type ConnectedL2Tunnel = Readonly<{
  sendFrame(frame: Uint8Array): void;
  close(): void;
}>;

type GatewaySessionResponse = Readonly<{
  endpoints?: Readonly<{
    l2?: string;
  }>;
  limits?: Readonly<{
    l2?: Readonly<{
      maxFramePayloadBytes: number;
      maxControlPayloadBytes: number;
    }>;
  }>;
  udpRelay?: Readonly<{
    baseUrl: string;
    token?: string;
  }>;
}>;

const DEFAULT_KEEPALIVE_MAX_MS = 15_000;
// Gateway session responses should be small JSON payloads; cap size to avoid pathological
// allocations if the gateway is misconfigured or attacker-controlled.
const MAX_GATEWAY_SESSION_RESPONSE_BYTES = 1024 * 1024; // 1 MiB

function parseGatewayBaseUrl(gatewayBaseUrl: string): URL {
  // `gatewayBaseUrl` is generally an absolute URL, but same-origin deployments may
  // pass a relative path like `/base`. Browsers can resolve this against
  // `location.href`; in Node test runners `location` is absent, so fall back to
  // `new URL(gatewayBaseUrl)` (which will throw on relative input).
  const baseHref = (globalThis as unknown as { location?: { href?: unknown } }).location?.href;
  return baseHref && typeof baseHref === "string" ? new URL(gatewayBaseUrl, baseHref) : new URL(gatewayBaseUrl);
}

function buildSessionUrl(gatewayBaseUrl: string): string {
  const url = parseGatewayBaseUrl(gatewayBaseUrl);

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

function buildWebSocketUrlFromEndpoint(gatewayBaseUrl: string, endpoint: string): string {
  const url = parseGatewayBaseUrl(gatewayBaseUrl);
  if (url.protocol === "http:") url.protocol = "ws:";
  if (url.protocol === "https:") url.protocol = "wss:";
  url.search = "";
  url.hash = "";

  // `gatewayBaseUrl` may already include `/l2` or legacy `/eth`.
  let basePath = url.pathname.replace(/\/$/, "");
  if (basePath.endsWith("/l2") || basePath.endsWith("/eth")) {
    basePath = basePath.replace(/\/(l2|eth)$/, "");
  }

  // The gateway session response returns path-like strings (typically beginning
  // with `/`). Treat them as relative to the configured gateway base path so
  // deployments behind a reverse-proxy prefix (`/base`) continue to work.
  //
  // Newer gateways will already include the base prefix in `endpoints.*` (e.g.
  // `/base/l2`); for compatibility with older gateways, avoid double-prefixing.
  const trimmedEndpoint = endpoint.trim();
  const normalizedBase = basePath.replace(/\/$/, "");

  if (
    normalizedBase.length > 0 &&
    trimmedEndpoint.startsWith("/") &&
    trimmedEndpoint.startsWith(`${normalizedBase}/`)
  ) {
    url.pathname = trimmedEndpoint;
    return url.toString();
  }

  const endpointPath = trimmedEndpoint.startsWith("/") ? trimmedEndpoint.slice(1) : trimmedEndpoint;
  url.pathname = `${normalizedBase}/${endpointPath}`.replace(/^\/\//, "/");

  return url.toString();
}

async function bootstrapSession(gatewayBaseUrl: string): Promise<string> {
  const res = await fetch(buildSessionUrl(gatewayBaseUrl), {
    method: "POST",
    credentials: "include",
    headers: { "content-type": "application/json" },
    body: "{}",
  });

  const text = await readTextResponseWithLimit(res, {
    maxBytes: MAX_GATEWAY_SESSION_RESPONSE_BYTES,
    label: "gateway session response",
  });
  if (!res.ok) {
    // Do not reflect response bodies in client-visible errors.
    throw new Error(`failed to bootstrap gateway session (${res.status})`);
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

  if (mode !== "ws" && mode !== "webrtc") {
    // Runtime guard for consumers that bypass TS typechecking.
    throw new Error(`unsupported L2 tunnel mode: ${String(mode)}`);
  }

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

  const keepaliveMaxMs = opts.tunnelOptions?.keepaliveMaxMs ?? DEFAULT_KEEPALIVE_MAX_MS;
  validateNonNegativeInt("keepaliveMaxMs", keepaliveMaxMs);
  const idleTimeoutMs = opts.idleTimeoutMs ?? (keepaliveMaxMs > 0 ? keepaliveMaxMs * 2 : 0);
  validateNonNegativeInt("idleTimeoutMs", idleTimeoutMs);

  let reconnectAttempts = 0;
  let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  let closed = false;
  let generation = 0;

  let currentSendFrame: ((frame: Uint8Array) => void) | null = null;
  let currentClose: (() => void) | null = null;
  let idleTimer: ReturnType<typeof setTimeout> | null = null;

  function clearIdleTimer(): void {
    if (idleTimer === null) return;
    clearTimeout(idleTimer);
    idleTimer = null;
  }

  function armIdleTimer(gen: number): void {
    clearIdleTimer();
    if (idleTimeoutMs <= 0) return;
    idleTimer = setTimeout(() => {
      if (closed) return;
      if (gen !== generation) return;
      emitErrorThrottled(new Error(`L2 tunnel idle timeout (${idleTimeoutMs}ms)`));
      // Force-close the current transport; the resulting `close` event will also
      // go through the normal reconnect path.
      teardownTunnel();
      scheduleReconnect();
    }, idleTimeoutMs);
    unrefBestEffort(idleTimer);
  }

  function installTunnel(sendFrame: (frame: Uint8Array) => void, close: () => void): void {
    currentSendFrame = sendFrame;
    currentClose = close;
  }

  function teardownTunnel(): void {
    clearIdleTimer();
    const close = currentClose;
    currentSendFrame = null;
    currentClose = null;
    try {
      close?.();
    } catch {
      // Ignore.
    }
  }

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
    unrefBestEffort(reconnectTimer);
  };

  const makeSink = (gen: number): L2TunnelSink => {
    return (ev) => {
      // Ignore late events from superseded tunnels.
      if (gen !== generation) return;

      if (ev.type === "error") {
        // `WebSocketL2TunnelClient` also throttles error events internally, but
        // `connectL2Tunnel` can emit its own errors (disconnects, reconnect
        // attempts, `sendFrame()` while disconnected). Run everything through the
        // same throttler so callers see at most one error per interval overall.
        emitErrorThrottled(ev.error);
        return;
      }

      if (ev.type === "open") {
        reconnectAttempts = 0;
        armIdleTimer(gen);
      } else if (ev.type === "frame" || ev.type === "pong") {
        armIdleTimer(gen);
      } else if (ev.type === "close") {
        // Drop references immediately so `sendFrame()` starts behaving like a
        // disconnected tunnel (rather than silently calling into a closed client).
        teardownTunnel();
        opts.sink(ev);
        scheduleReconnect();
        return;
      }

      opts.sink(ev);
    };
  };

  const sessionText = await bootstrapSession(gatewayBaseUrl);
  let initialSession: GatewaySessionResponse | null = null;
  try {
    initialSession = parseGatewaySessionResponse(sessionText);
  } catch (err) {
    if (mode === "webrtc") {
      throw new Error("invalid gateway session response JSON");
    }
  }

  const computeTunnelOptions = (session: GatewaySessionResponse | null): L2TunnelClientOptions => {
    const out: L2TunnelClientOptions = { ...(opts.tunnelOptions ?? {}) };
    if (out.maxFrameSize === undefined) {
      const maxFramePayloadBytes = session?.limits?.l2?.maxFramePayloadBytes;
      if (typeof maxFramePayloadBytes === "number" && Number.isInteger(maxFramePayloadBytes) && maxFramePayloadBytes > 0) {
        out.maxFrameSize = maxFramePayloadBytes;
      }
    }
    if (out.maxControlSize === undefined) {
      const maxControlPayloadBytes = session?.limits?.l2?.maxControlPayloadBytes;
      if (
        typeof maxControlPayloadBytes === "number" &&
        Number.isInteger(maxControlPayloadBytes) &&
        maxControlPayloadBytes > 0
      ) {
        out.maxControlSize = maxControlPayloadBytes;
      }
    }
    return out;
  };

  const computeWsBaseUrl = (session: GatewaySessionResponse | null): string => {
    const endpoint = session?.endpoints?.l2;
    return typeof endpoint === "string" && endpoint.length > 0
      ? buildWebSocketUrlFromEndpoint(gatewayBaseUrl, endpoint)
      : gatewayBaseUrl;
  };

  const reconnectNow = async () => {
    if (closed) return;
    teardownTunnel();

    const gen = generation + 1;
    generation = gen;

    if (mode === "ws") {
      let nextSessionText: string;
      try {
        nextSessionText = await bootstrapSession(gatewayBaseUrl);
      } catch (err) {
        emitErrorThrottled(err);
        scheduleReconnect();
        return;
      }
      if (closed) return;

      let nextSession: GatewaySessionResponse | null = null;
      try {
        nextSession = parseGatewaySessionResponse(nextSessionText);
      } catch {
        // Session response parsing is best-effort for ws mode; fall back to
        // gatewayBaseUrl when the JSON is absent or malformed.
      }

      const l2 = new WebSocketL2TunnelClient(computeWsBaseUrl(nextSession), makeSink(gen), computeTunnelOptions(nextSession));
      l2.connect();
      if (closed) {
        l2.close();
        return;
      }
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
      if (closed) return;

      let session: GatewaySessionResponse;
      try {
        session = parseGatewaySessionResponse(nextSessionText);
      } catch (err) {
        emitErrorThrottled(new Error(formatOneLineError(err, 256, "invalid gateway session response JSON")));
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

      const tunnelOptions = computeTunnelOptions(session);

      try {
        const { l2, close } = await connectL2Relay({
          baseUrl: relay.baseUrl,
          authToken: relay.token,
          mode: opts.relaySignalingMode,
          sink: makeSink(gen),
          tunnelOptions,
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
    const l2 = new WebSocketL2TunnelClient(computeWsBaseUrl(initialSession), makeSink(generation), computeTunnelOptions(initialSession));
    l2.connect();
    installTunnel((frame) => l2.sendFrame(frame), () => l2.close());
  }

  if (mode === "webrtc") {
    const relay = initialSession?.udpRelay;
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
      tunnelOptions: computeTunnelOptions(initialSession),
    });
    installTunnel((frame) => l2.sendFrame(frame), close);
  }

  const tunnel: ConnectedL2Tunnel = {
    sendFrame(frame: Uint8Array): void {
      // Drop outbound frames silently when the tunnel is disconnected/reconnecting.
      // Callers should treat `close`/`open` events as the authoritative connection
      // state signal.
      currentSendFrame?.(frame);
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
