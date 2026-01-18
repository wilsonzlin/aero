import {
  L2_TUNNEL_DEFAULT_MAX_CONTROL_PAYLOAD,
  L2_TUNNEL_DEFAULT_MAX_FRAME_PAYLOAD,
  L2_TUNNEL_DATA_CHANNEL_LABEL,
  L2_TUNNEL_ERROR_CODE_AUTH_INVALID,
  L2_TUNNEL_ERROR_CODE_AUTH_REQUIRED,
  L2_TUNNEL_ERROR_CODE_BACKPRESSURE,
  L2_TUNNEL_ERROR_CODE_ORIGIN_DENIED,
  L2_TUNNEL_ERROR_CODE_ORIGIN_MISSING,
  L2_TUNNEL_ERROR_CODE_PROTOCOL_ERROR,
  L2_TUNNEL_ERROR_CODE_QUOTA_BYTES,
  L2_TUNNEL_ERROR_CODE_QUOTA_CONNECTIONS,
  L2_TUNNEL_ERROR_CODE_QUOTA_FPS,
  L2_TUNNEL_TYPE_ERROR,
  L2_TUNNEL_TYPE_FRAME,
  L2_TUNNEL_TYPE_PING,
  L2_TUNNEL_TYPE_PONG,
  L2_TUNNEL_SUBPROTOCOL,
  L2_TUNNEL_TOKEN_SUBPROTOCOL_PREFIX,
  decodeStructuredErrorPayload,
  decodeL2Message,
  encodeL2Frame,
  encodePing,
  encodePong,
} from "../shared/l2TunnelProtocol";
import { dcBufferedAmountSafe, dcCloseSafe, dcIsOpenSafe, dcSendSafe } from "./rtcSafe";
import { wsBufferedAmountSafe, wsCloseSafe, wsIsClosedSafe, wsIsOpenSafe, wsProtocolSafe, wsSendSafe } from "./wsSafe.ts";

export { L2_TUNNEL_SUBPROTOCOL, L2_TUNNEL_DATA_CHANNEL_LABEL };

// RFC 6455: Sec-WebSocket-Protocol values must be HTTP "tokens".
// https://www.rfc-editor.org/rfc/rfc6455#section-4.1
const WEBSOCKET_SUBPROTOCOL_TOKEN_RE = /^[!#$%&'*+\-.^_`|~0-9A-Za-z]+$/;

export function assertL2TunnelDataChannelSemantics(channel: RTCDataChannel): void {
  if (channel.label !== L2_TUNNEL_DATA_CHANNEL_LABEL) {
    throw new Error(`expected DataChannel label=${L2_TUNNEL_DATA_CHANNEL_LABEL} (got ${channel.label})`);
  }
  // The proxy-side stack assumes guest TCP segments arrive in order. A reliable
  // but unordered DataChannel can deliver messages out of order under loss, so
  // we require `ordered=true` for correctness.
  if (!channel.ordered) {
    throw new Error(`l2 DataChannel must be ordered (ordered=true)`);
  }
  if (channel.maxRetransmits != null) {
    throw new Error(`l2 DataChannel must be fully reliable (maxRetransmits must be unset)`);
  }
  if (channel.maxPacketLifeTime != null) {
    throw new Error(`l2 DataChannel must be fully reliable (maxPacketLifeTime must be unset)`);
  }
}

export function createL2TunnelDataChannel(pc: RTCPeerConnection): RTCDataChannel {
  // L2 tunnel MUST be reliable and ordered. Do not set maxRetransmits /
  // maxPacketLifeTime (partial reliability).
  const channel = pc.createDataChannel(L2_TUNNEL_DATA_CHANNEL_LABEL, { ordered: true });
  assertL2TunnelDataChannelSemantics(channel);
  return channel;
}

export type L2TunnelEvent =
  | { type: "open" }
  | { type: "frame"; frame: Uint8Array }
  | { type: "close"; code?: number }
  | { type: "error"; error: unknown }
  | { type: "pong"; rttMs?: number };

export type L2TunnelSink = (ev: L2TunnelEvent) => void;

export type L2TunnelTokenTransport = "query" | "subprotocol" | "both";

export type L2TunnelClientOptions = {
  /**
   * Maximum number of bytes allowed to be queued in JS before the tunnel is
   * closed.
   */
  maxQueuedBytes?: number;

  /**
   * Backpressure threshold for the underlying transport (`WebSocket` or
   * `RTCDataChannel`): when `bufferedAmount` exceeds this, the client pauses
   * sending and waits for the transport to drain (messages remain queued, up to
   * `maxQueuedBytes`).
   */
  maxBufferedAmount?: number;

  /**
   * Maximum Ethernet frame size allowed.
   *
   * Outbound frames larger than this are treated as an error and the tunnel is
   * closed (to avoid silent frame loss).
   */
  maxFrameSize?: number;

  /**
   * Maximum payload size for L2 tunnel control messages (PING/PONG/ERROR).
   *
   * This should generally match `POST /session` `limits.l2.maxControlPayloadBytes`
   * (or the proxy's configured control payload limit). The default is the
   * protocol recommended value.
   *
   * Note: The L2 tunnel framing uses a fixed 4-byte header; this limit applies
   * only to the message payload bytes.
   */
  maxControlSize?: number;

  /**
   * When emitting errors (queue overflow, oversize), emit at most one `{ type:
   * "error" }` event per interval to avoid spamming.
   */
  errorIntervalMs?: number;

  /**
   * Keepalive PING interval range. A randomized value in this range is picked
   * each time to avoid synchronized thundering herds.
   *
   * Set both to 0 to disable keepalive.
   */
  keepaliveMinMs?: number;
  keepaliveMaxMs?: number;

  /**
   * Optional token for deployments that require auth on the `/l2` WebSocket
   * endpoint.
   */
  token?: string;

  /**
   * How to transport `token` to the server (WebSocket only).
   *
   * - `"query"` (default): send `?token=<token>` (legacy servers).
   * - `"subprotocol"`: offer an additional `Sec-WebSocket-Protocol` entry
   *   `aero-l2-token.<token>` alongside `aero-l2-tunnel-v1` to avoid leaking
   *   tokens via URLs/logs/referrers. The negotiated subprotocol must still be
   *   `aero-l2-tunnel-v1`.
   * - `"both"`: send both mechanisms for compatibility during migrations.
   *
   * Note: `Sec-WebSocket-Protocol` values must be valid HTTP "tokens" (RFC
   * 6455). If your token contains spaces or other disallowed characters, use
   * `"query"` or a header-safe token format (e.g. base64url/JWT).
   */
  tokenTransport?: L2TunnelTokenTransport;

  /**
   * @deprecated Use `tokenTransport` instead.
   *
   * - `true`  => `tokenTransport: "subprotocol"`
   * - `false` => `tokenTransport: "query"`
   */
  tokenViaSubprotocol?: boolean;
};

export interface L2TunnelClient {
  /**
   * Establish the underlying transport.
   *
   * Note: `WebRtcL2TunnelClient` is effectively connected once its
   * `RTCDataChannel` is open, so this method is a no-op there.
   */
  connect(): void;
  /**
   * Enqueue an outbound Ethernet frame.
   *
   * Returns `true` if the frame was accepted into the tunnel client's outbound
   * queue, or `false` if it was dropped/refused (e.g. client closed or not
   * connected).
   *
   * Callers that need backpressure telemetry (e.g. `L2TunnelForwarder`) can use
   * this return value; most callers can ignore it.
   */
  sendFrame(frame: Uint8Array): boolean;
  close(): void;
}

type RequiredOptions = {
  maxQueuedBytes: number;
  maxBufferedAmount: number;
  maxFrameSize: number;
  maxControlSize: number;
  errorIntervalMs: number;
  keepaliveMinMs: number;
  keepaliveMaxMs: number;
};

const DEFAULT_MAX_QUEUED_BYTES = 8 * 1024 * 1024;
const DEFAULT_MAX_BUFFERED_AMOUNT = 16 * 1024 * 1024;
const DEFAULT_MAX_FRAME_SIZE = L2_TUNNEL_DEFAULT_MAX_FRAME_PAYLOAD;
const DEFAULT_MAX_CONTROL_SIZE = L2_TUNNEL_DEFAULT_MAX_CONTROL_PAYLOAD;
const DEFAULT_ERROR_INTERVAL_MS = 1000;
const DEFAULT_KEEPALIVE_MIN_MS = 5000;
const DEFAULT_KEEPALIVE_MAX_MS = 15000;
const DEFAULT_DRAIN_RETRY_MS = 10;

function nowMs(): number {
  // `performance.now()` is the preferred clock for RTT; fall back for non-browser
  // environments (e.g. vitest/node).
  if (typeof performance !== "undefined" && typeof performance.now === "function") {
    return performance.now();
  }
  return Date.now();
}

function validateNonNegativeInt(name: string, value: number): void {
  if (!Number.isInteger(value) || value < 0) {
    throw new RangeError(`${name} must be a non-negative integer (got ${value})`);
  }
}

function decodePeerErrorCode(payload: Uint8Array): number | undefined {
  return decodeStructuredErrorPayload(payload)?.code;
}

function formatPeerErrorMessage(code: number | undefined, payloadByteLength: number): string {
  if (code === undefined) {
    // Unstructured error payloads are untrusted; do not decode or reflect bytes.
    return `l2 tunnel error (unstructured payload, ${payloadByteLength} bytes)`;
  }

  // Structured codes are stable; do not reflect peer-provided message strings.
  switch (code) {
    case L2_TUNNEL_ERROR_CODE_PROTOCOL_ERROR:
      return `l2 tunnel error (${code}): protocol error`;
    case L2_TUNNEL_ERROR_CODE_AUTH_REQUIRED:
      return `l2 tunnel error (${code}): authentication required`;
    case L2_TUNNEL_ERROR_CODE_AUTH_INVALID:
      return `l2 tunnel error (${code}): invalid authentication token`;
    case L2_TUNNEL_ERROR_CODE_ORIGIN_MISSING:
      return `l2 tunnel error (${code}): missing Origin`;
    case L2_TUNNEL_ERROR_CODE_ORIGIN_DENIED:
      return `l2 tunnel error (${code}): Origin denied`;
    case L2_TUNNEL_ERROR_CODE_QUOTA_BYTES:
      return `l2 tunnel error (${code}): quota exceeded (bytes)`;
    case L2_TUNNEL_ERROR_CODE_QUOTA_FPS:
      return `l2 tunnel error (${code}): quota exceeded (fps)`;
    case L2_TUNNEL_ERROR_CODE_QUOTA_CONNECTIONS:
      return `l2 tunnel error (${code}): quota exceeded (connections)`;
    case L2_TUNNEL_ERROR_CODE_BACKPRESSURE:
      return `l2 tunnel error (${code}): backpressure`;
    default:
      return `l2 tunnel error (${code})`;
  }
}

abstract class BaseL2TunnelClient implements L2TunnelClient {
  protected readonly sink: L2TunnelSink;
  protected readonly opts: RequiredOptions;
  protected readonly token: string | undefined;
  protected readonly tokenTransport: L2TunnelTokenTransport;

  private sendQueue: Uint8Array[] = [];
  private sendQueueHead = 0;
  private sendQueueBytes = 0;

  private flushScheduled = false;
  private drainRetryTimer: ReturnType<typeof setTimeout> | null = null;
  private keepaliveTimer: ReturnType<typeof setTimeout> | null = null;

  private lastErrorEmitAt = 0;

  private nextPingNonce = (Math.random() * 0xffff_ffff) >>> 0;
  private pendingPings = new Map<number, number>();
  private lastInboundAtMs = 0;
  // Never close for keepalive timeout before we've emitted at least one ping on this session.
  private sentKeepalivePing = false;
  // For keepalive timeouts, ensure we attempt at least one ping since the last inbound message
  // before closing due to extended inbound silence. This avoids prematurely closing in
  // environments with coarse timer resolution (and makes unit tests with very small keepalive
  // intervals stable).
  private keepaliveSentSinceLastInbound = false;

  private opened = false;
  private closed = false;
  private closing = false;

  constructor(
    sink: L2TunnelSink,
    opts: L2TunnelClientOptions = {},
  ) {
    this.sink = sink;
    const maxQueuedBytes = opts.maxQueuedBytes ?? DEFAULT_MAX_QUEUED_BYTES;
    const maxBufferedAmount = opts.maxBufferedAmount ?? DEFAULT_MAX_BUFFERED_AMOUNT;
    const maxFrameSize = opts.maxFrameSize ?? DEFAULT_MAX_FRAME_SIZE;
    const maxControlSize = opts.maxControlSize ?? DEFAULT_MAX_CONTROL_SIZE;
    const errorIntervalMs = opts.errorIntervalMs ?? DEFAULT_ERROR_INTERVAL_MS;
    const keepaliveMinMs = opts.keepaliveMinMs ?? DEFAULT_KEEPALIVE_MIN_MS;
    const keepaliveMaxMs = opts.keepaliveMaxMs ?? DEFAULT_KEEPALIVE_MAX_MS;

    validateNonNegativeInt("maxQueuedBytes", maxQueuedBytes);
    validateNonNegativeInt("maxBufferedAmount", maxBufferedAmount);
    validateNonNegativeInt("maxFrameSize", maxFrameSize);
    validateNonNegativeInt("maxControlSize", maxControlSize);
    validateNonNegativeInt("errorIntervalMs", errorIntervalMs);
    validateNonNegativeInt("keepaliveMinMs", keepaliveMinMs);
    validateNonNegativeInt("keepaliveMaxMs", keepaliveMaxMs);

    if (keepaliveMinMs > keepaliveMaxMs) {
      throw new RangeError(`keepaliveMinMs must be <= keepaliveMaxMs (${keepaliveMinMs} > ${keepaliveMaxMs})`);
    }

    this.opts = { maxQueuedBytes, maxBufferedAmount, maxFrameSize, maxControlSize, errorIntervalMs, keepaliveMinMs, keepaliveMaxMs };
    this.token = opts.token;
    const tokenTransport = opts.tokenTransport ?? (opts.tokenViaSubprotocol ? "subprotocol" : "query");
    if (tokenTransport !== "query" && tokenTransport !== "subprotocol" && tokenTransport !== "both") {
      throw new RangeError(
        `tokenTransport must be "query", "subprotocol", or "both" (got ${JSON.stringify(tokenTransport)})`,
      );
    }
    this.tokenTransport = tokenTransport;
  }

  connect(): void {
    // No-op by default; used by WebSocket client.
  }

  sendFrame(frame: Uint8Array): boolean {
    if (this.closed || this.closing) return false;
    if (!this.canEnqueue()) {
      this.emitSessionErrorThrottled(new Error("L2 tunnel is not connected; call connect() first"));
      return false;
    }

    if (frame.byteLength > this.opts.maxFrameSize) {
      const err = new Error(
        `closing L2 tunnel: outbound frame too large (size ${frame.byteLength} > maxFrameSize ${this.opts.maxFrameSize})`,
      );
      this.emitSessionErrorThrottled(err);
      this.close();
      return false;
    }

    return this.enqueue(encodeL2Frame(frame, { maxPayload: this.opts.maxFrameSize }));
  }

  close(): void {
    if (this.closed || this.closing) return;
    this.closing = true;
    this.stopKeepalive();
    this.clearDrainRetryTimer();
    this.clearQueue();
    this.closeTransport();
  }

  protected abstract canEnqueue(): boolean;
  protected abstract isTransportOpen(): boolean;
  protected abstract getTransportBufferedAmount(): number;
  protected abstract transportSend(data: Uint8Array): void;
  protected abstract closeTransport(): void;

  protected onTransportOpen(): void {
    if (this.closed || this.closing || this.opened) return;
    this.opened = true;
    this.lastInboundAtMs = nowMs();
    this.sentKeepalivePing = false;
    this.keepaliveSentSinceLastInbound = false;
    this.sink({ type: "open" });
    this.startKeepalive();
    this.scheduleFlush();
  }

  protected onTransportClose(code?: number, reason?: string): void {
    if (this.closed) return;
    this.closed = true;
    this.lastInboundAtMs = 0;

    this.stopKeepalive();
    this.clearDrainRetryTimer();
    this.clearQueue();

    // Close reasons are peer-controlled; do not surface them to callers by default.
    this.sink({ type: "close", code });
  }

  protected onTransportError(error: unknown): void {
    if (this.closed || this.closing) return;
    this.sink({ type: "error", error });
  }

  protected onTransportMessage(data: Uint8Array): void {
    if (this.closed || this.closing) return;
    const receivedAt = nowMs();
    let msg;
    try {
      msg = decodeL2Message(data, { maxFramePayload: this.opts.maxFrameSize, maxControlPayload: this.opts.maxControlSize });
    } catch (err) {
      // Malformed/unexpected control messages should not kill the session.
      this.emitSessionErrorThrottled(err);
      return;
    }
    this.lastInboundAtMs = receivedAt;
    this.keepaliveSentSinceLastInbound = false;

    if (msg.type === L2_TUNNEL_TYPE_FRAME) {
      this.sink({ type: "frame", frame: msg.payload });
      return;
    }

    if (msg.type === L2_TUNNEL_TYPE_PING) {
      // Respond immediately; do not surface to callers. Payload is opaque and is
      // echoed back in the PONG for correlation/RTT measurements.
      this.enqueue(encodePong(msg.payload, { maxPayload: this.opts.maxControlSize }));
      return;
    }

    if (msg.type === L2_TUNNEL_TYPE_PONG) {
      const nonce = this.decodePingNonce(msg.payload);
      if (nonce === null) {
        this.sink({ type: "pong" });
        return;
      }

      const sentAt = this.pendingPings.get(nonce);
      if (sentAt === undefined) {
        this.sink({ type: "pong" });
        return;
      }
      this.pendingPings.delete(nonce);
      this.sink({ type: "pong", rttMs: nowMs() - sentAt });
      return;
    }

    if (msg.type === L2_TUNNEL_TYPE_ERROR) {
      const code = decodePeerErrorCode(msg.payload);
      this.onTransportError(new Error(formatPeerErrorMessage(code, msg.payload.byteLength)));
      return;
    }

    // Unknown control message: drop.
  }

  protected onTransportWritable(): void {
    // A transport (RTCDataChannel) can call this when its internal buffer drains
    // (e.g. via `onbufferedamountlow`).
    this.scheduleFlush();
  }

  private enqueue(msg: Uint8Array): boolean {
    if (this.sendQueueBytes + msg.byteLength > this.opts.maxQueuedBytes) {
      this.emitSessionErrorThrottled(
        new Error(
          `closing L2 tunnel: send queue overflow (${this.sendQueueBytes} + ${msg.byteLength} > ${this.opts.maxQueuedBytes})`,
        ),
      );
      this.close();
      return false;
    }

    this.sendQueue.push(msg);
    this.sendQueueBytes += msg.byteLength;
    this.scheduleFlush();
    return true;
  }

  private clearQueue(): void {
    this.sendQueue = [];
    this.sendQueueHead = 0;
    this.sendQueueBytes = 0;
  }

  private scheduleFlush(): void {
    if (this.flushScheduled) return;
    this.flushScheduled = true;
    queueMicrotask(() => {
      this.flushScheduled = false;
      this.flush();
    });
  }

  private flush(): void {
    if (this.closed || this.closing || !this.isTransportOpen()) return;
    this.clearDrainRetryTimer();

    while (this.sendQueueHead < this.sendQueue.length) {
      if (this.getTransportBufferedAmount() > this.opts.maxBufferedAmount) {
        this.scheduleDrainRetry();
        return;
      }

      const msg = this.sendQueue[this.sendQueueHead]!;
      this.sendQueueHead += 1;
      this.sendQueueBytes -= msg.byteLength;

      try {
        this.transportSend(msg);
      } catch (err) {
        this.emitSessionErrorThrottled(err);
        this.close();
        return;
      }
    }

    // Reclaim memory once we've drained the queue.
    if (this.sendQueueHead > 0) {
      this.sendQueue = [];
      this.sendQueueHead = 0;
    }
  }

  private scheduleDrainRetry(): void {
    if (this.drainRetryTimer !== null) return;
    this.drainRetryTimer = setTimeout(() => {
      this.drainRetryTimer = null;
      this.scheduleFlush();
    }, DEFAULT_DRAIN_RETRY_MS);
    // In Node-based test runners (Vitest), referenced timers can keep the process alive after a
    // test finishes. Browsers return a numeric handle, so use a safe cast.
    (this.drainRetryTimer as unknown as { unref?: () => void }).unref?.();
  }

  private clearDrainRetryTimer(): void {
    if (this.drainRetryTimer === null) return;
    clearTimeout(this.drainRetryTimer);
    this.drainRetryTimer = null;
  }

  private startKeepalive(): void {
    if (this.opts.keepaliveMaxMs <= 0) return;
    this.scheduleNextPing();
  }

  private stopKeepalive(): void {
    if (this.keepaliveTimer !== null) {
      clearTimeout(this.keepaliveTimer);
      this.keepaliveTimer = null;
    }
    this.pendingPings.clear();
    this.sentKeepalivePing = false;
    this.keepaliveSentSinceLastInbound = false;
  }

  private scheduleNextPing(): void {
    if (this.keepaliveTimer !== null) return;
    if (this.opts.keepaliveMaxMs <= 0) return;

    // Uniform random within [min, max].
    const span = this.opts.keepaliveMaxMs - this.opts.keepaliveMinMs;
    const delay = this.opts.keepaliveMinMs + Math.floor(Math.random() * (span + 1));

    this.keepaliveTimer = setTimeout(() => {
      this.keepaliveTimer = null;
      this.sendPing();
    }, delay);
    (this.keepaliveTimer as unknown as { unref?: () => void }).unref?.();
  }

  private sendPing(): void {
    if (this.closed || this.closing || !this.isTransportOpen()) {
      // If the transport isn't open, keepalive will restart on the next open.
      return;
    }

    const sentAt = nowMs();
    // Never close the tunnel before we've emitted at least one keepalive ping. In slower
    // environments (sandboxed CI / heavily loaded test runners) the first timer callback can be
    // delayed beyond the nominal interval, and we still want to attempt a ping before declaring a
    // hard timeout.
    if (this.sentKeepalivePing && this.opts.keepaliveMaxMs > 0 && this.lastInboundAtMs > 0) {
      const idleTimeoutMs = this.opts.keepaliveMaxMs * 2;
      if (idleTimeoutMs > 0 && sentAt - this.lastInboundAtMs > idleTimeoutMs) {
        // If we've already sent a keepalive ping since the last inbound message, treat this as a
        // real timeout. Otherwise, send one more ping first (the next keepalive interval will
        // close if the peer still doesn't respond).
        if (this.keepaliveSentSinceLastInbound) {
          this.emitSessionErrorThrottled(new Error(`closing L2 tunnel: keepalive timeout (${idleTimeoutMs}ms)`));
          this.close();
          return;
        }
      }
    }

    const canSendNonce = this.opts.maxControlSize >= 4;
    const pingPayload = canSendNonce ? this.encodePingNonce(this.nextPingNonce) : new Uint8Array();
    if (canSendNonce) {
      const nonce = this.nextPingNonce;
      this.nextPingNonce = (this.nextPingNonce + 1) >>> 0;
      this.pendingPings.set(nonce, sentAt);
      // Bound map growth if the peer never responds.
      if (this.pendingPings.size > 16) {
        const first = this.pendingPings.keys().next().value as number;
        this.pendingPings.delete(first);
      }
    }

    this.keepaliveSentSinceLastInbound = true;
    this.enqueue(encodePing(pingPayload, { maxPayload: this.opts.maxControlSize }));
    this.sentKeepalivePing = true;
    this.scheduleNextPing();
  }

  private encodePingNonce(nonce: number): Uint8Array {
    const payload = new Uint8Array(4);
    new DataView(payload.buffer).setUint32(0, nonce >>> 0, false);
    return payload;
  }

  private decodePingNonce(payload: Uint8Array): number | null {
    if (payload.byteLength < 4) return null;
    return new DataView(payload.buffer, payload.byteOffset, payload.byteLength).getUint32(0, false);
  }

  private emitSessionErrorThrottled(error: unknown): void {
    const now = Date.now();
    if (this.opts.errorIntervalMs > 0 && now - this.lastErrorEmitAt < this.opts.errorIntervalMs) return;
    this.lastErrorEmitAt = now;
    this.sink({ type: "error", error });
  }

  protected isClosedOrClosing(): boolean {
    return this.closed || this.closing;
  }
}

/**
 * Browser-side L2 tunnel client over WebSocket.
 *
 * Usage:
 *
 * ```ts
 * import { WebSocketL2TunnelClient } from "./net";
 *
 * // `gatewayBaseUrl` may be `https://...` (auto-converted to `wss://.../l2`),
 * // or an explicit `wss://.../l2` URL.
 * const tunnel = new WebSocketL2TunnelClient("https://gateway.example.com", (ev) => {
 *   if (ev.type === "frame") nicRx(ev.frame);
 * });
 *
 * tunnel.connect();
 * nicTx = (frame) => {
 *   // `sendFrame()` returns a boolean indicating whether the frame was accepted
 *   // into the client's outbound queue; most callers can ignore it.
 *   tunnel.sendFrame(frame);
 * };
 * ```
 */
export class WebSocketL2TunnelClient extends BaseL2TunnelClient {
  private ws: WebSocket | null = null;
  private readonly gatewayBaseUrl: string;

  constructor(
    gatewayBaseUrl: string,
    sink: L2TunnelSink,
    opts: L2TunnelClientOptions = {},
  ) {
    super(sink, opts);
    this.gatewayBaseUrl = gatewayBaseUrl;

    if (this.token !== undefined && this.tokenTransport !== "query") {
      const proto = `${L2_TUNNEL_TOKEN_SUBPROTOCOL_PREFIX}${this.token}`;
      if (!WEBSOCKET_SUBPROTOCOL_TOKEN_RE.test(proto)) {
        throw new RangeError(
          `token contains characters not valid for Sec-WebSocket-Protocol; ` +
            `use tokenTransport="query" or a header-safe token (len=${this.token.length})`,
        );
      }
    }
  }

  connect(): void {
    if (this.isClosedOrClosing()) return;
    if (this.ws && !wsIsClosedSafe(this.ws)) return;

    const ws = new WebSocket(this.buildWebSocketUrl(), this.buildWebSocketProtocols());
    ws.binaryType = "arraybuffer";

    ws.onopen = () => {
      // `docs/l2-tunnel-protocol.md` requires strict subprotocol negotiation.
      const negotiated = wsProtocolSafe(ws) ?? "";
      if (negotiated !== L2_TUNNEL_SUBPROTOCOL) {
        this.onTransportError(
          new Error(
            `L2 tunnel subprotocol not negotiated (wanted ${L2_TUNNEL_SUBPROTOCOL}, got ${negotiated || "none"})`,
          ),
        );
        wsCloseSafe(ws, 1002);
        return;
      }
      this.onTransportOpen();
    };
    ws.onmessage = (evt) => {
      if (!(evt.data instanceof ArrayBuffer)) return;
      this.onTransportMessage(new Uint8Array(evt.data));
    };
    ws.onerror = (err) => this.onTransportError(err);
    ws.onclose = (evt) => {
      this.ws = null;
      this.onTransportClose(evt.code);
    };

    this.ws = ws;
  }

  private buildWebSocketProtocols(): string | string[] {
    if (this.token === undefined) return L2_TUNNEL_SUBPROTOCOL;
    if (this.tokenTransport === "query") return L2_TUNNEL_SUBPROTOCOL;
    return [L2_TUNNEL_SUBPROTOCOL, `${L2_TUNNEL_TOKEN_SUBPROTOCOL_PREFIX}${this.token}`];
  }

  private buildWebSocketUrl(): string {
    const baseHref = (globalThis as unknown as { location?: { href?: unknown } }).location?.href;
    const url = baseHref && typeof baseHref === "string" ? new URL(this.gatewayBaseUrl, baseHref) : new URL(this.gatewayBaseUrl);
    if (url.protocol === "http:") url.protocol = "ws:";
    if (url.protocol === "https:") url.protocol = "wss:";

    // Accept either a base URL (append `/l2`) or an explicit endpoint URL that
    // already points at `/l2`/`/eth`.
    const path = url.pathname.replace(/\/$/, "");
    if (path.endsWith("/l2") || path.endsWith("/eth")) {
      url.pathname = path;
    } else {
      url.pathname = `${path}/l2`;
    }

    if (this.token !== undefined) {
      // Avoid accidentally sending multiple/conflicting credential parameters when callers pass
      // a base URL that already contains legacy `token`/`apiKey` query params.
      //
      // Note: `crates/aero-l2-proxy` checks query-string credentials before any `aero-l2-token.*`
      // subprotocol token, so leaving an old `token`/`apiKey` value on the URL can override a new
      // credential and cause auth failures.
      url.searchParams.delete("apiKey");
      if (this.tokenTransport === "subprotocol") {
        // If the caller provided `token` and requested subprotocol transport,
        // ensure we do not leak/override with a `?token=` value that may already
        // exist on the base URL.
        url.searchParams.delete("token");
      } else {
        url.searchParams.set("token", this.token);
      }
    }

    return url.toString();
  }

  protected canEnqueue(): boolean {
    return this.ws !== null;
  }

  protected isTransportOpen(): boolean {
    return wsIsOpenSafe(this.ws);
  }

  protected getTransportBufferedAmount(): number {
    return wsBufferedAmountSafe(this.ws);
  }

  protected transportSend(data: Uint8Array): void {
    const ws = this.ws;
    if (!ws) return;
    if (!wsSendSafe(ws, data)) {
      this.onTransportError(new Error("l2 tunnel websocket send failed"));
      this.close();
    }
  }

  protected closeTransport(): void {
    if (this.ws) wsCloseSafe(this.ws);
    this.ws = null;
  }
}

/**
 * Browser-side L2 tunnel client over WebRTC `RTCDataChannel`.
 *
 * The caller is responsible for signaling / ICE negotiation and should pass an
 * already-created data channel.
 *
 * Required channel options:
 * - `ordered: true`
 * - do NOT set `maxRetransmits` or `maxPacketLifeTime` (fully reliable)
 *
 * See `docs/adr/0013-networking-l2-tunnel.md` and `docs/l2-tunnel-protocol.md`.
 */
export class WebRtcL2TunnelClient extends BaseL2TunnelClient {
  private readonly channel: RTCDataChannel;

  constructor(
    channel: RTCDataChannel,
    sink: L2TunnelSink,
    opts: L2TunnelClientOptions = {},
  ) {
    super(sink, opts);
    this.channel = channel;

    assertL2TunnelDataChannelSemantics(channel);
    channel.binaryType = "arraybuffer";
    channel.bufferedAmountLowThreshold = Math.floor(this.opts.maxBufferedAmount / 2);
    channel.onbufferedamountlow = () => this.onTransportWritable();

    channel.onopen = () => this.onTransportOpen();
    channel.onmessage = (evt) => {
      if (!(evt.data instanceof ArrayBuffer)) return;
      this.onTransportMessage(new Uint8Array(evt.data));
    };
    channel.onclose = () => this.onTransportClose();
    channel.onerror = (err) => this.onTransportError(err);

    // If the channel is already open, `onopen` won't fire again.
    queueMicrotask(() => {
      if (!dcIsOpenSafe(channel)) return;
      this.onTransportOpen();
    });
  }

  // WebRTC connections are established externally; `connect()` is a no-op.
  connect(): void {}

  protected canEnqueue(): boolean {
    return true;
  }

  protected isTransportOpen(): boolean {
    return dcIsOpenSafe(this.channel);
  }

  protected getTransportBufferedAmount(): number {
    return dcBufferedAmountSafe(this.channel);
  }

  protected transportSend(data: Uint8Array): void {
    // Some lib.dom versions model `RTCDataChannel.send()` as accepting only
    // `ArrayBuffer`-backed views. At runtime, we still prefer avoiding copies,
    // but ensure the type is compatible (and avoid passing SharedArrayBuffer-
    // backed views to APIs that may not accept them).
    const view: Uint8Array<ArrayBuffer> =
      data.buffer instanceof ArrayBuffer ? (data as unknown as Uint8Array<ArrayBuffer>) : new Uint8Array(data);
    if (!dcSendSafe(this.channel, view)) {
      this.onTransportError(new Error("RTCDataChannel send failed"));
      this.close();
    }
  }

  protected closeTransport(): void {
    dcCloseSafe(this.channel);
  }
}
