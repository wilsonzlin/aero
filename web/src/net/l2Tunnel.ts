import {
  L2_TUNNEL_TYPE_ERROR,
  L2_TUNNEL_TYPE_FRAME,
  L2_TUNNEL_TYPE_PING,
  L2_TUNNEL_TYPE_PONG,
  decodeL2Message,
  encodeL2Frame,
  encodePing,
  encodePong,
} from "../shared/l2TunnelProtocol";

export const L2_TUNNEL_SUBPROTOCOL = "aero-l2-tunnel-v1";
export const L2_TUNNEL_DATA_CHANNEL_LABEL = "l2";

export function assertL2TunnelDataChannelSemantics(channel: RTCDataChannel): void {
  if (channel.label !== L2_TUNNEL_DATA_CHANNEL_LABEL) {
    throw new Error(`expected DataChannel label=${L2_TUNNEL_DATA_CHANNEL_LABEL} (got ${channel.label})`);
  }
  // Ordering is optional; some deployments prefer ordered delivery for more
  // predictable throughput, while others use unordered delivery to reduce
  // head-of-line blocking.
  if (channel.maxRetransmits != null) {
    throw new Error(`l2 DataChannel must be fully reliable (maxRetransmits must be unset)`);
  }
  if (channel.maxPacketLifeTime != null) {
    throw new Error(`l2 DataChannel must be fully reliable (maxPacketLifeTime must be unset)`);
  }
}

export function createL2TunnelDataChannel(pc: RTCPeerConnection): RTCDataChannel {
  // L2 tunnel MUST be reliable. Ordering is optional; we default to ordered
  // delivery for more predictable throughput.
  const channel = pc.createDataChannel(L2_TUNNEL_DATA_CHANNEL_LABEL, { ordered: true });
  assertL2TunnelDataChannelSemantics(channel);
  return channel;
}

const textDecoder = new TextDecoder();

export type L2TunnelEvent =
  | { type: "open" }
  | { type: "frame"; frame: Uint8Array }
  | { type: "close"; code?: number; reason?: string }
  | { type: "error"; error: unknown }
  | { type: "pong"; rttMs?: number };

export type L2TunnelSink = (ev: L2TunnelEvent) => void;

export type L2TunnelClientOptions = {
  /**
   * Maximum number of bytes allowed to be queued in JS before outbound frames
   * are dropped.
   */
  maxQueuedBytes?: number;

  /**
   * If the underlying transport's buffered amount exceeds this, the client will
   * drop outbound frames.
   */
  maxBufferedAmount?: number;

  /**
   * Drop outbound Ethernet frames larger than this.
   */
  maxFrameSize?: number;

  /**
   * When emitting errors (queue overflow, oversize), emit at most one `{ type:
   * "error" }` event per interval to avoid spamming.
   */
  errorIntervalMs?: number;

  /**
   * Keepalive PING interval range. A randomized value in this range is picked
   * each time to avoid synchronized thundering herds.
   */
  keepaliveMinMs?: number;
  keepaliveMaxMs?: number;

  /**
   * Optional token for deployments that require `?token=...` on the `/l2`
   * WebSocket URL.
   */
  token?: string;
};

export interface L2TunnelClient {
  /**
   * Establish the underlying transport.
   *
   * Note: `WebRtcL2TunnelClient` is effectively connected once its
   * `RTCDataChannel` is open, so this method is a no-op there.
   */
  connect(): void;
  sendFrame(frame: Uint8Array): void;
  close(): void;
}

type RequiredOptions = {
  maxQueuedBytes: number;
  maxBufferedAmount: number;
  maxFrameSize: number;
  errorIntervalMs: number;
  keepaliveMinMs: number;
  keepaliveMaxMs: number;
};

const DEFAULT_MAX_QUEUED_BYTES = 8 * 1024 * 1024;
const DEFAULT_MAX_BUFFERED_AMOUNT = 16 * 1024 * 1024;
const DEFAULT_MAX_FRAME_SIZE = 2048;
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

function decodePeerErrorPayload(payload: Uint8Array): { code?: number; message: string } {
  // Prefer the structured binary form from docs/l2-tunnel-protocol.md:
  //   code (u16 BE) | msg_len (u16 BE) | msg (msg_len bytes, UTF-8)
  if (payload.byteLength >= 4) {
    const dv = new DataView(payload.buffer, payload.byteOffset, payload.byteLength);
    const code = dv.getUint16(0, false);
    const msgLen = dv.getUint16(2, false);
    if (payload.byteLength === 4 + msgLen) {
      const msgBytes = payload.subarray(4);
      try {
        return { code, message: textDecoder.decode(msgBytes) };
      } catch {
        // Fall through to the unstructured decoding below.
      }
    }
  }

  // Unstructured form: treat the entire payload as UTF-8 (best-effort).
  try {
    return { message: textDecoder.decode(payload) };
  } catch {
    return { message: `l2 tunnel error (${payload.byteLength} bytes)` };
  }
}

abstract class BaseL2TunnelClient implements L2TunnelClient {
  protected readonly opts: RequiredOptions;
  protected readonly token: string | undefined;

  private sendQueue: Uint8Array[] = [];
  private sendQueueHead = 0;
  private sendQueueBytes = 0;

  private flushScheduled = false;
  private drainRetryTimer: ReturnType<typeof setTimeout> | null = null;
  private keepaliveTimer: ReturnType<typeof setTimeout> | null = null;

  private lastErrorEmitAt = 0;

  private nextPingNonce = (Math.random() * 0xffff_ffff) >>> 0;
  private pendingPings = new Map<number, number>();

  private opened = false;
  private closed = false;
  private closing = false;

  constructor(
    protected readonly sink: L2TunnelSink,
    opts: L2TunnelClientOptions = {},
  ) {
    const maxQueuedBytes = opts.maxQueuedBytes ?? DEFAULT_MAX_QUEUED_BYTES;
    const maxBufferedAmount = opts.maxBufferedAmount ?? DEFAULT_MAX_BUFFERED_AMOUNT;
    const maxFrameSize = opts.maxFrameSize ?? DEFAULT_MAX_FRAME_SIZE;
    const errorIntervalMs = opts.errorIntervalMs ?? DEFAULT_ERROR_INTERVAL_MS;
    const keepaliveMinMs = opts.keepaliveMinMs ?? DEFAULT_KEEPALIVE_MIN_MS;
    const keepaliveMaxMs = opts.keepaliveMaxMs ?? DEFAULT_KEEPALIVE_MAX_MS;

    validateNonNegativeInt("maxQueuedBytes", maxQueuedBytes);
    validateNonNegativeInt("maxBufferedAmount", maxBufferedAmount);
    validateNonNegativeInt("maxFrameSize", maxFrameSize);
    validateNonNegativeInt("errorIntervalMs", errorIntervalMs);
    validateNonNegativeInt("keepaliveMinMs", keepaliveMinMs);
    validateNonNegativeInt("keepaliveMaxMs", keepaliveMaxMs);

    if (keepaliveMinMs > keepaliveMaxMs) {
      throw new RangeError(`keepaliveMinMs must be <= keepaliveMaxMs (${keepaliveMinMs} > ${keepaliveMaxMs})`);
    }

    this.opts = { maxQueuedBytes, maxBufferedAmount, maxFrameSize, errorIntervalMs, keepaliveMinMs, keepaliveMaxMs };
    this.token = opts.token;
  }

  connect(): void {
    // No-op by default; used by WebSocket client.
  }

  sendFrame(frame: Uint8Array): void {
    if (this.closed || this.closing) return;
    if (!this.canEnqueue()) {
      this.emitSessionErrorThrottled(new Error("L2 tunnel is not connected; call connect() first"));
      return;
    }

    if (frame.byteLength > this.opts.maxFrameSize) {
      this.emitSessionErrorThrottled(
        new Error(`dropping outbound frame: size ${frame.byteLength} > maxFrameSize ${this.opts.maxFrameSize}`),
      );
      return;
    }

    if (this.isTransportOpen() && this.getTransportBufferedAmount() > this.opts.maxBufferedAmount) {
      this.emitSessionErrorThrottled(
        new Error(
          `dropping outbound frame: transport backpressure (bufferedAmount ${this.getTransportBufferedAmount()} > maxBufferedAmount ${this.opts.maxBufferedAmount})`,
        ),
      );
      return;
    }

    this.enqueue(encodeL2Frame(frame, { maxPayload: this.opts.maxFrameSize }));
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
    this.sink({ type: "open" });
    this.startKeepalive();
    this.scheduleFlush();
  }

  protected onTransportClose(code?: number, reason?: string): void {
    if (this.closed) return;
    this.closed = true;

    this.stopKeepalive();
    this.clearDrainRetryTimer();
    this.clearQueue();

    this.sink({ type: "close", code, reason });
  }

  protected onTransportError(error: unknown): void {
    if (this.closed || this.closing) return;
    this.sink({ type: "error", error });
  }

  protected onTransportMessage(data: Uint8Array): void {
    if (this.closed || this.closing) return;
    let msg;
    try {
      msg = decodeL2Message(data, { maxFramePayload: this.opts.maxFrameSize });
    } catch (err) {
      // Malformed/unexpected control messages should not kill the session.
      this.emitSessionErrorThrottled(err);
      return;
    }

    if (msg.type === L2_TUNNEL_TYPE_FRAME) {
      this.sink({ type: "frame", frame: msg.payload });
      return;
    }

    if (msg.type === L2_TUNNEL_TYPE_PING) {
      // Respond immediately; do not surface to callers. Payload is opaque and is
      // echoed back in the PONG for correlation/RTT measurements.
      this.enqueue(encodePong(msg.payload));
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
      const decoded = decodePeerErrorPayload(msg.payload);
      const prefix = decoded.code === undefined ? "l2 tunnel error" : `l2 tunnel error (${decoded.code})`;
      this.onTransportError(new Error(`${prefix}: ${decoded.message}`));
      return;
    }

    // Unknown control message: drop.
  }

  protected onTransportWritable(): void {
    // A transport (RTCDataChannel) can call this when its internal buffer drains
    // (e.g. via `onbufferedamountlow`).
    this.scheduleFlush();
  }

  private enqueue(msg: Uint8Array): void {
    if (this.sendQueueBytes + msg.byteLength > this.opts.maxQueuedBytes) {
      this.emitSessionErrorThrottled(
        new Error(
          `dropping outbound message: send queue overflow (${this.sendQueueBytes} + ${msg.byteLength} > ${this.opts.maxQueuedBytes})`,
        ),
      );
      return;
    }

    this.sendQueue.push(msg);
    this.sendQueueBytes += msg.byteLength;
    this.scheduleFlush();
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
  }

  private clearDrainRetryTimer(): void {
    if (this.drainRetryTimer === null) return;
    clearTimeout(this.drainRetryTimer);
    this.drainRetryTimer = null;
  }

  private startKeepalive(): void {
    this.scheduleNextPing();
  }

  private stopKeepalive(): void {
    if (this.keepaliveTimer !== null) {
      clearTimeout(this.keepaliveTimer);
      this.keepaliveTimer = null;
    }
    this.pendingPings.clear();
  }

  private scheduleNextPing(): void {
    if (this.keepaliveTimer !== null) return;

    // Uniform random within [min, max].
    const span = this.opts.keepaliveMaxMs - this.opts.keepaliveMinMs;
    const delay = this.opts.keepaliveMinMs + Math.floor(Math.random() * (span + 1));

    this.keepaliveTimer = setTimeout(() => {
      this.keepaliveTimer = null;
      this.sendPing();
    }, delay);
  }

  private sendPing(): void {
    if (this.closed || this.closing || !this.isTransportOpen()) {
      // If the transport isn't open, keepalive will restart on the next open.
      return;
    }

    const nonce = this.nextPingNonce;
    this.nextPingNonce = (this.nextPingNonce + 1) >>> 0;
    this.pendingPings.set(nonce, nowMs());
    // Bound map growth if the peer never responds.
    if (this.pendingPings.size > 16) {
      const first = this.pendingPings.keys().next().value as number;
      this.pendingPings.delete(first);
    }

    this.enqueue(encodePing(this.encodePingNonce(nonce)));
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
 * nicTx = (frame) => tunnel.sendFrame(frame);
 * ```
 */
export class WebSocketL2TunnelClient extends BaseL2TunnelClient {
  private ws: WebSocket | null = null;

  constructor(
    private readonly gatewayBaseUrl: string,
    sink: L2TunnelSink,
    opts: L2TunnelClientOptions = {},
  ) {
    super(sink, opts);
  }

  connect(): void {
    if (this.isClosedOrClosing()) return;
    if (this.ws && this.ws.readyState !== WebSocket.CLOSED) return;

    const ws = new WebSocket(this.buildWebSocketUrl(), L2_TUNNEL_SUBPROTOCOL);
    ws.binaryType = "arraybuffer";

    ws.onopen = () => {
      // `docs/l2-tunnel-protocol.md` requires strict subprotocol negotiation.
      if (ws.protocol !== L2_TUNNEL_SUBPROTOCOL) {
        this.onTransportError(
          new Error(
            `L2 tunnel subprotocol not negotiated (wanted ${L2_TUNNEL_SUBPROTOCOL}, got ${ws.protocol || "none"})`,
          ),
        );
        ws.close(1002);
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
      this.onTransportClose(evt.code, evt.reason);
    };

    this.ws = ws;
  }

  private buildWebSocketUrl(): string {
    const url = new URL(this.gatewayBaseUrl);
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
      url.searchParams.set("token", this.token);
    }

    return url.toString();
  }

  protected canEnqueue(): boolean {
    return this.ws !== null;
  }

  protected isTransportOpen(): boolean {
    return this.ws?.readyState === WebSocket.OPEN;
  }

  protected getTransportBufferedAmount(): number {
    return this.ws?.bufferedAmount ?? 0;
  }

  protected transportSend(data: Uint8Array): void {
    this.ws?.send(data);
  }

  protected closeTransport(): void {
    this.ws?.close();
    this.ws = null;
  }
}

/**
 * Browser-side L2 tunnel client over WebRTC `RTCDataChannel`.
 *
 * The caller is responsible for signaling / ICE negotiation and should pass an
 * already-created data channel.
 *
 * Recommended channel options for low-latency forwarding:
 * - `ordered: true` (recommended default; unordered is OK)
 * - do NOT set `maxRetransmits` or `maxPacketLifeTime` (fully reliable)
 *
 * See `docs/adr/0013-networking-l2-tunnel.md` and `docs/l2-tunnel-protocol.md`.
 */
export class WebRtcL2TunnelClient extends BaseL2TunnelClient {
  constructor(
    private readonly channel: RTCDataChannel,
    sink: L2TunnelSink,
    opts: L2TunnelClientOptions = {},
  ) {
    super(sink, opts);

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
    if (channel.readyState === "open") {
      queueMicrotask(() => this.onTransportOpen());
    }
  }

  // WebRTC connections are established externally; `connect()` is a no-op.
  connect(): void {}

  protected canEnqueue(): boolean {
    return true;
  }

  protected isTransportOpen(): boolean {
    return this.channel.readyState === "open";
  }

  protected getTransportBufferedAmount(): number {
    return this.channel.bufferedAmount;
  }

  protected transportSend(data: Uint8Array): void {
    // Some lib.dom versions model `RTCDataChannel.send()` as accepting only
    // `ArrayBuffer`-backed views. At runtime, we still prefer avoiding copies,
    // but ensure the type is compatible (and avoid passing SharedArrayBuffer-
    // backed views to APIs that may not accept them).
    const view: Uint8Array<ArrayBuffer> =
      data.buffer instanceof ArrayBuffer ? (data as unknown as Uint8Array<ArrayBuffer>) : new Uint8Array(data);
    this.channel.send(view);
  }

  protected closeTransport(): void {
    this.channel.close();
  }
}
