// Browser-side multiplexed TCP proxy client for `/tcp-mux`.
//
// Speaks the canonical `aero-tcp-mux-v1` framing used by:
// - `backend/aero-gateway` (production)
// - `tools/net-proxy-server` (dev relay)

import { buildWebSocketUrl } from "./wsUrl.ts";
import type { NetTracer } from "./net_tracer.ts";
import { formatOneLineError, formatOneLineUtf8 } from "../text.ts";
import { wsBufferedAmountSafe, wsCloseSafe, wsSendSafe } from "./wsSafe.ts";

export const TCP_MUX_SUBPROTOCOL = "aero-tcp-mux-v1";

export const TCP_MUX_HEADER_BYTES = 9;

// Defensive caps: keep aligned with the gateway and relay implementations.
export const MAX_TCP_MUX_OPEN_HOST_BYTES = 1024;
export const MAX_TCP_MUX_OPEN_METADATA_BYTES = 4 * 1024;
export const MAX_TCP_MUX_ERROR_MESSAGE_BYTES = 1024;

// NOTE: This file is executed directly by Node's `--experimental-strip-types`
// loader in unit tests. Node's "strip-only" TypeScript support does not handle
// TS `enum`, so we use runtime objects + type aliases instead.

export const TcpMuxMsgType = {
  OPEN: 1,
  DATA: 2,
  CLOSE: 3,
  ERROR: 4,
  PING: 5,
  PONG: 6,
} as const;
export type TcpMuxMsgType = (typeof TcpMuxMsgType)[keyof typeof TcpMuxMsgType];
export const TcpMuxCloseFlags = {
  FIN: 0x01,
  RST: 0x02,
} as const;
// Close flags are a bitmask (FIN | RST), so keep the type permissive.
export type TcpMuxCloseFlags = number;

// Matches `backend/aero-gateway/src/protocol/tcpMux.ts`.
export const TcpMuxErrorCode = {
  POLICY_DENIED: 1,
  DIAL_FAILED: 2,
  PROTOCOL_ERROR: 3,
  UNKNOWN_STREAM: 4,
  STREAM_LIMIT_EXCEEDED: 5,
  STREAM_BUFFER_OVERFLOW: 6,
} as const;

export type TcpMuxErrorCode = (typeof TcpMuxErrorCode)[keyof typeof TcpMuxErrorCode];

export type TcpMuxFrame = Readonly<{
  msgType: TcpMuxMsgType;
  streamId: number;
  payload: Uint8Array;
}>;

export type TcpMuxError = Readonly<{
  code: number;
  message: string;
}>;

const MAX_CLIENT_ERROR_MESSAGE_BYTES = 512;

const textEncoder = new TextEncoder();
const utf8DecoderFatal = new TextDecoder("utf-8", { fatal: true });

function decodeUtf8Exact(bytes: Uint8Array, context: string): string {
  try {
    return utf8DecoderFatal.decode(bytes);
  } catch {
    throw new Error(`${context} is not valid UTF-8`);
  }
}

function hasControlOrWhitespace(value: string): boolean {
  for (const ch of value) {
    const code = ch.codePointAt(0) ?? 0;
    const forbidden =
      code <= 0x1f || code === 0x7f || code === 0x85 || code === 0x2028 || code === 0x2029;
    if (forbidden || /\s/u.test(ch)) return true;
  }
  return false;
}

function formatClientErrorMessage(input: unknown, fallback: string): string {
  return formatOneLineError(input, MAX_CLIENT_ERROR_MESSAGE_BYTES, fallback);
}

function concatBytes(a: Uint8Array, b: Uint8Array): Uint8Array {
  if (a.byteLength === 0) return b;
  if (b.byteLength === 0) return a;
  const out = new Uint8Array(a.byteLength + b.byteLength);
  out.set(a, 0);
  out.set(b, a.byteLength);
  return out;
}

export function encodeTcpMuxFrame(msgType: TcpMuxMsgType, streamId: number, payload?: Uint8Array): Uint8Array {
  if (!Number.isInteger(streamId) || streamId < 0 || streamId > 0xffffffff) {
    throw new Error("invalid streamId");
  }
  const payloadBytes = payload ?? new Uint8Array(0);
  const buf = new Uint8Array(TCP_MUX_HEADER_BYTES + payloadBytes.byteLength);
  const dv = new DataView(buf.buffer);
  dv.setUint8(0, msgType);
  dv.setUint32(1, streamId >>> 0, false);
  dv.setUint32(5, payloadBytes.byteLength >>> 0, false);
  buf.set(payloadBytes, TCP_MUX_HEADER_BYTES);
  return buf;
}

export class TcpMuxFrameParser {
  private buffer: Uint8Array = new Uint8Array(0);
  private readonly maxPayloadBytes: number;

  constructor(opts: { maxPayloadBytes?: number } = {}) {
    this.maxPayloadBytes = opts.maxPayloadBytes ?? 16 * 1024 * 1024;
  }

  push(chunk: Uint8Array): TcpMuxFrame[] {
    if (chunk.byteLength === 0) return [];
    this.buffer = concatBytes(this.buffer, chunk);

    const frames: TcpMuxFrame[] = [];

    while (this.buffer.byteLength >= TCP_MUX_HEADER_BYTES) {
      const dv = new DataView(this.buffer.buffer, this.buffer.byteOffset, this.buffer.byteLength);
      const msgType = dv.getUint8(0) as TcpMuxMsgType;
      const streamId = dv.getUint32(1, false);
      const length = dv.getUint32(5, false);

      if (length > this.maxPayloadBytes) {
        throw new Error(`frame payload too large: ${length} > ${this.maxPayloadBytes}`);
      }

      const totalBytes = TCP_MUX_HEADER_BYTES + length;
      if (this.buffer.byteLength < totalBytes) break;

      const payload = this.buffer.subarray(TCP_MUX_HEADER_BYTES, totalBytes);
      frames.push({ msgType, streamId, payload });

      this.buffer = this.buffer.subarray(totalBytes);
      if (this.buffer.byteLength === 0) {
        // Avoid keeping the original (potentially large) backing ArrayBuffer alive
        // via a zero-length view during idle periods.
        this.buffer = new Uint8Array(0);
      }
    }

    // If we're buffering more than a header + max payload, the stream is
    // malformed (or peer is attempting to OOM us). Fail fast.
    if (this.buffer.byteLength > TCP_MUX_HEADER_BYTES + this.maxPayloadBytes) {
      throw new Error("tcp-mux internal buffer overflow");
    }

    return frames;
  }

  pendingBytes(): number {
    return this.buffer.byteLength;
  }

  finish(): void {
    if (this.buffer.byteLength === 0) return;
    throw new Error(`truncated tcp-mux frame stream (${this.buffer.byteLength} pending bytes)`);
  }
}

export type TcpMuxOpenPayload = Readonly<{
  host: string;
  port: number;
  metadata?: string;
}>;

export function encodeTcpMuxOpenPayload(payload: TcpMuxOpenPayload): Uint8Array {
  const hostBytes = textEncoder.encode(payload.host);
  const metadataBytes = payload.metadata ? textEncoder.encode(payload.metadata) : new Uint8Array(0);

  if (hostBytes.byteLength > MAX_TCP_MUX_OPEN_HOST_BYTES) {
    throw new Error("host too long");
  }
  if (metadataBytes.byteLength > MAX_TCP_MUX_OPEN_METADATA_BYTES) {
    throw new Error("metadata too long");
  }
  if (!Number.isInteger(payload.port) || payload.port < 1 || payload.port > 65535) {
    throw new Error("invalid port");
  }

  const buf = new Uint8Array(2 + hostBytes.byteLength + 2 + 2 + metadataBytes.byteLength);
  const dv = new DataView(buf.buffer);
  let offset = 0;
  dv.setUint16(offset, hostBytes.byteLength, false);
  offset += 2;
  buf.set(hostBytes, offset);
  offset += hostBytes.byteLength;
  dv.setUint16(offset, payload.port, false);
  offset += 2;
  dv.setUint16(offset, metadataBytes.byteLength, false);
  offset += 2;
  buf.set(metadataBytes, offset);
  return buf;
}

export function decodeTcpMuxOpenPayload(buf: Uint8Array): TcpMuxOpenPayload {
  if (buf.byteLength < 2 + 2 + 2) {
    throw new Error("OPEN payload too short");
  }

  let offset = 0;
  const dv = new DataView(buf.buffer, buf.byteOffset, buf.byteLength);
  const hostLen = dv.getUint16(offset, false);
  offset += 2;

  if (hostLen > MAX_TCP_MUX_OPEN_HOST_BYTES) {
    throw new Error("host too long");
  }
  if (buf.byteLength < offset + hostLen + 2 + 2) {
    throw new Error("OPEN payload truncated (host)");
  }

  const host = decodeUtf8Exact(buf.subarray(offset, offset + hostLen), "host");
  if (!host) throw new Error("host is empty");
  if (hasControlOrWhitespace(host)) throw new Error("invalid host");
  offset += hostLen;

  const port = dv.getUint16(offset, false);
  offset += 2;

  const metadataLen = dv.getUint16(offset, false);
  offset += 2;

  if (metadataLen > MAX_TCP_MUX_OPEN_METADATA_BYTES) {
    throw new Error("metadata too long");
  }
  if (buf.byteLength < offset + metadataLen) {
    throw new Error("OPEN payload truncated (metadata)");
  }

  const metadata =
    metadataLen > 0 ? decodeUtf8Exact(buf.subarray(offset, offset + metadataLen), "metadata") : undefined;
  offset += metadataLen;

  if (offset !== buf.byteLength) {
    throw new Error("OPEN payload has trailing bytes");
  }

  return { host, port, metadata };
}

export function encodeTcpMuxClosePayload(flags: number): Uint8Array {
  const buf = new Uint8Array(1);
  buf[0] = flags & 0xff;
  return buf;
}

export function decodeTcpMuxClosePayload(payload: Uint8Array): { flags: number } {
  if (payload.byteLength !== 1) {
    throw new Error("CLOSE payload must be exactly 1 byte");
  }
  return { flags: payload[0]! };
}

export function encodeTcpMuxErrorPayload(code: TcpMuxErrorCode | number, message: string): Uint8Array {
  const safeMessage = formatOneLineUtf8(message, MAX_TCP_MUX_ERROR_MESSAGE_BYTES);
  const messageBytes = textEncoder.encode(safeMessage);
  if (messageBytes.byteLength > MAX_TCP_MUX_ERROR_MESSAGE_BYTES) {
    throw new Error("error message too long");
  }
  const buf = new Uint8Array(2 + 2 + messageBytes.byteLength);
  const dv = new DataView(buf.buffer);
  dv.setUint16(0, code & 0xffff, false);
  dv.setUint16(2, messageBytes.byteLength, false);
  buf.set(messageBytes, 4);
  return buf;
}

export function decodeTcpMuxErrorPayload(payload: Uint8Array): TcpMuxError {
  if (payload.byteLength < 4) throw new Error("ERROR payload too short");
  const dv = new DataView(payload.buffer, payload.byteOffset, payload.byteLength);
  const code = dv.getUint16(0, false);
  const msgLen = dv.getUint16(2, false);
  if (msgLen > MAX_TCP_MUX_ERROR_MESSAGE_BYTES) {
    throw new Error("error message too long");
  }
  if (payload.byteLength !== 4 + msgLen) {
    throw new Error("ERROR payload length mismatch");
  }
  const msgBytes = payload.subarray(4, 4 + msgLen);
  const messageRaw = decodeUtf8Exact(msgBytes, "message");
  const message = formatOneLineUtf8(messageRaw, MAX_TCP_MUX_ERROR_MESSAGE_BYTES);
  return { code, message };
}

export type TcpMuxProxyOptions = Readonly<{
  /**
   * Optional `?token=` query parameter for the dev relay (`tools/net-proxy-server`).
   *
   * NOTE: This is **not** the same as the production Aero Gateway's cookie-backed
   * sessions (`POST /session`, `aero_session` cookie).
   */
  authToken?: string;

  /**
   * Maximum number of bytes allowed to be queued in JS before we start rejecting
   * writes.
   */
  maxQueuedBytes?: number;

  /**
   * If `WebSocket.bufferedAmount` exceeds this, we stop flushing the queue
   * until it drains again.
   */
  maxBufferedAmount?: number;

  /**
   * Max payload bytes per DATA frame.
   *
   * Large writes are chunked to avoid intermediary limits and to improve
   * fairness across multiplexed streams.
   */
  maxDataChunkBytes?: number;

  /**
   * Maximum payload bytes accepted for any incoming frame. Exceeding this is
   * treated as a protocol error.
   */
  maxIncomingFramePayloadBytes?: number;

  /**
   * When we pause flushing due to `WebSocket.bufferedAmount` backpressure, we
   * poll on this interval until it drains.
   */
  bufferedAmountPollMs?: number;

  /**
   * Optional network tracer hook (best-effort).
   */
  tracer?: NetTracer;
}>;

type StreamState = {
  openedNotified: boolean;
  closeNotified: boolean;
  localFin: boolean;
  remoteFin: boolean;
  closed: boolean;
};

type QueuedFrame = {
  msgType: TcpMuxMsgType;
  streamId: number;
  frame: Uint8Array;
};

export class WebSocketTcpMuxProxyClient {
  /**
   * Fired once per stream.
   *
   * Note: `aero-tcp-mux-v1` does not have an explicit OPEN acknowledgement.
   * `onOpen` is fired once we have locally accepted the OPEN request (i.e. after
   * enqueueing it). If the gateway rejects the stream, an `ERROR` frame will
   * arrive later (and we guarantee `onOpen` runs before `onError` for that
   * stream).
   */
  onOpen?: (streamId: number) => void;
  onData?: (streamId: number, data: Uint8Array) => void;

  /**
   * Fired once per stream.
   *
   * Important: `CLOSE(FIN)` is a *half-close* (the peer will not send more data,
   * but may still accept writes). This callback is invoked on remote FIN *and*
   * when we fully tear down a stream locally (RST/ERROR/session close). It is
   * only fired once; if a stream half-closes and later fully closes, callers
   * will not get a second notification.
   *
   * Callers that need "fully closed" semantics should treat a local
   * `close(..., { fin: true })` as the point where they stop writing.
   */
  onClose?: (streamId: number) => void;
  onError?: (streamId: number, error: TcpMuxError) => void;

  private readonly ws: WebSocket;
  private readonly parser: TcpMuxFrameParser;
  readonly closed: Promise<void>;

  private readonly streams = new Map<number, StreamState>();
  private readonly usedStreamIds = new Set<number>();

  private queued: QueuedFrame[] = [];
  private queuedHead = 0;
  private queuedBytes = 0;
  private flushScheduled = false;

  private readonly maxQueuedBytes: number;
  private readonly maxBufferedAmount: number;
  private readonly maxDataChunkBytes: number;
  private readonly bufferedAmountPollMs: number;
  private readonly tracer?: NetTracer;

  constructor(gatewayBaseUrl: string, opts: TcpMuxProxyOptions = {}) {
    this.maxQueuedBytes = opts.maxQueuedBytes ?? 4 * 1024 * 1024;
    this.maxBufferedAmount = opts.maxBufferedAmount ?? 8 * 1024 * 1024;
    this.maxDataChunkBytes = opts.maxDataChunkBytes ?? 16 * 1024;
    this.bufferedAmountPollMs = opts.bufferedAmountPollMs ?? 10;
    this.tracer = opts.tracer;

    this.parser = new TcpMuxFrameParser({
      maxPayloadBytes: opts.maxIncomingFramePayloadBytes ?? 16 * 1024 * 1024,
    });

    const url = buildWebSocketUrl(gatewayBaseUrl, "/tcp-mux");
    if (opts.authToken) {
      url.searchParams.set("token", opts.authToken);
    }

    this.ws = new WebSocket(url.toString(), TCP_MUX_SUBPROTOCOL);
    this.ws.binaryType = "arraybuffer";

    this.ws.onopen = () => {
      // `aero-tcp-mux-v1` requires strict subprotocol negotiation. If the peer
      // doesn't select it, close immediately to avoid speaking the wrong framing
      // protocol on an otherwise-open WebSocket.
      if (this.ws.protocol !== TCP_MUX_SUBPROTOCOL) {
        this.onError?.(0, {
          code: 0,
          message: `tcp-mux subprotocol not negotiated (wanted ${TCP_MUX_SUBPROTOCOL}, got ${this.ws.protocol || "none"})`,
        });
        wsCloseSafe(this.ws, 1002);
        return;
      }
      this.scheduleFlush(0);
    };
    this.ws.onmessage = (evt) => this.onWsMessage(evt);
    this.ws.onerror = () => {
      // Browser WebSocket errors do not expose details; treat as session-level.
      this.onError?.(0, { code: 0, message: "WebSocket error" });
    };
    this.ws.onclose = () => this.onWsClose();

    this.closed = new Promise((resolve) => this.ws.addEventListener("close", () => resolve(), { once: true }));
  }

  open(streamId: number, host: string, port: number, metadata?: string): void {
    if (streamId === 0) {
      this.onError?.(0, { code: 0, message: "stream_id=0 is reserved" });
      return;
    }
    if (this.streams.has(streamId)) return;
    if (this.usedStreamIds.has(streamId)) {
      this.onError?.(streamId, { code: TcpMuxErrorCode.PROTOCOL_ERROR, message: "stream_id was already used" });
      return;
    }

    this.streams.set(streamId, {
      openedNotified: false,
      closeNotified: false,
      localFin: false,
      remoteFin: false,
      closed: false,
    });

    try {
      const payload = encodeTcpMuxOpenPayload({ host, port, metadata });
      const frame = encodeTcpMuxFrame(TcpMuxMsgType.OPEN, streamId, payload);
      this.enqueue(TcpMuxMsgType.OPEN, streamId, frame);
    } catch (err) {
      this.maybeNotifyOpen(streamId);
      this.onError?.(streamId, { code: 0, message: formatClientErrorMessage(err, "Invalid OPEN request") });
      this.closeStream(streamId);
      return;
    }

    if (!this.streams.has(streamId)) return;
    this.usedStreamIds.add(streamId);

    // There is no explicit OPEN-OK in the v1 protocol; success is implicit.
    // Callers may send DATA immediately; the gateway buffers until the TCP dial
    // completes.
    this.maybeNotifyOpen(streamId);
  }

  send(streamId: number, bytes: Uint8Array): void {
    const st = this.streams.get(streamId);
    if (!st || st.closed || st.localFin) return;
    if (bytes.byteLength === 0) return;

    for (let off = 0; off < bytes.byteLength; off += this.maxDataChunkBytes) {
      const chunk = bytes.subarray(off, Math.min(bytes.byteLength, off + this.maxDataChunkBytes));
      try {
        const frame = encodeTcpMuxFrame(TcpMuxMsgType.DATA, streamId, chunk);
        this.enqueue(TcpMuxMsgType.DATA, streamId, frame);
        if (st.closed) return;
        try {
          this.tracer?.recordTcpProxy("guest_to_remote", streamId, chunk);
        } catch {
          // Best-effort tracing: never interfere with proxy traffic.
        }
      } catch (err) {
        this.maybeNotifyOpen(streamId);
        this.onError?.(streamId, { code: 0, message: formatClientErrorMessage(err, "Invalid DATA frame") });
        this.closeStream(streamId);
        return;
      }
    }
  }

  close(streamId: number, mode: { fin?: true; rst?: true } = { fin: true }): void {
    const st = this.streams.get(streamId);
    if (!st || st.closed) return;

    const flags = mode.rst ? TcpMuxCloseFlags.RST : TcpMuxCloseFlags.FIN;
    if ((flags & TcpMuxCloseFlags.FIN) !== 0 && st.localFin) return;

    try {
      const frame = encodeTcpMuxFrame(TcpMuxMsgType.CLOSE, streamId, encodeTcpMuxClosePayload(flags));
      this.enqueue(TcpMuxMsgType.CLOSE, streamId, frame);
    } catch (err) {
      this.maybeNotifyOpen(streamId);
      this.onError?.(streamId, { code: 0, message: formatClientErrorMessage(err, "Invalid CLOSE frame") });
      this.closeStream(streamId);
      return;
    }

    if ((flags & TcpMuxCloseFlags.RST) !== 0) {
      // The gateway does not send an explicit CLOSE ack for RST; treat as
      // locally closed as soon as we enqueue it.
      this.closeStream(streamId, { keepQueuedClose: true });
      return;
    }

    st.localFin = true;
    if (st.remoteFin) {
      // Both directions have sent FIN; we can drop local state as soon as our
      // CLOSE(FIN) frame is enqueued.
      this.closeStream(streamId, { keepQueuedClose: true });
    }
  }

  shutdown(): Promise<void> {
    wsCloseSafe(this.ws);
    return this.closed;
  }

  private enqueue(msgType: TcpMuxMsgType, streamId: number, frame: Uint8Array): void {
    if (this.queuedBytes + frame.byteLength > this.maxQueuedBytes) {
      // Local backpressure/overflow: fail the stream (or session) immediately.
      this.maybeNotifyOpen(streamId);
      this.onError?.(streamId, { code: 0, message: "tcp-mux send queue overflow" });
      this.closeStream(streamId);
      return;
    }

    this.queued.push({ msgType, streamId, frame });
    this.queuedBytes += frame.byteLength;
    this.scheduleFlush(0);
  }

  private scheduleFlush(delayMs: number): void {
    if (this.flushScheduled) return;
    this.flushScheduled = true;

    const run = () => {
      this.flushScheduled = false;
      this.flush();
    };

    if (delayMs <= 0) {
      queueMicrotask(run);
    } else {
      const timer = setTimeout(run, delayMs);
      (timer as unknown as { unref?: () => void }).unref?.();
    }
  }

  private flush(): void {
    try {
      if (this.ws.readyState !== WebSocket.OPEN) return;
    } catch {
      return;
    }
    while (this.queuedHead < this.queued.length) {
      if (wsBufferedAmountSafe(this.ws) > this.maxBufferedAmount) {
        // Let the browser drain the socket; we'll try again shortly.
        this.scheduleFlush(this.bufferedAmountPollMs);
        return;
      }

      const entry = this.queued[this.queuedHead]!;
      this.queuedHead += 1;
      this.queuedBytes -= entry.frame.byteLength;
      if (!wsSendSafe(this.ws, entry.frame)) {
        this.onError?.(0, { code: 0, message: "WebSocket send failed" });
        // Trigger `onWsClose`, which tears down stream state.
        wsCloseSafe(this.ws);
        return;
      }
    }

    if (this.queuedHead > 0) {
      // Reclaim memory once we've drained (or largely drained) the queue.
      if (this.queuedHead >= this.queued.length) {
        this.queued = [];
        this.queuedHead = 0;
      } else if (this.queuedHead > 1024) {
        this.queued = this.queued.slice(this.queuedHead);
        this.queuedHead = 0;
      }
    }
  }

  private onWsMessage(evt: MessageEvent): void {
    if (!(evt.data instanceof ArrayBuffer)) {
      this.onError?.(0, { code: 0, message: "tcp-mux received non-binary WebSocket message" });
      wsCloseSafe(this.ws, 1003);
      return;
    }

    let frames: TcpMuxFrame[];
    try {
      frames = this.parser.push(new Uint8Array(evt.data));
    } catch (err) {
      this.onError?.(0, { code: 0, message: "tcp-mux protocol error" });
      wsCloseSafe(this.ws, 1002);
      return;
    }

    for (const frame of frames) {
      this.handleMuxFrame(frame);
    }
  }

  private onWsClose(): void {
    const pending = this.parser.pendingBytes();
    if (pending !== 0) {
      this.onError?.(0, {
        code: 0,
        message: formatOneLineUtf8(`tcp-mux connection closed with ${pending} unparsed bytes`, MAX_CLIENT_ERROR_MESSAGE_BYTES),
      });
    }

    for (const streamId of this.streams.keys()) {
      this.maybeNotifyOpen(streamId);
      this.onError?.(streamId, { code: 0, message: "Proxy session closed" });
      this.closeStream(streamId);
    }
    this.streams.clear();

    this.queued = [];
    this.queuedHead = 0;
    this.queuedBytes = 0;
  }

  private handleMuxFrame(frame: TcpMuxFrame): void {
    switch (frame.msgType) {
      case TcpMuxMsgType.DATA: {
        const st = this.streams.get(frame.streamId);
        if (!st || st.closed) return;
        this.maybeNotifyOpen(frame.streamId);
        try {
          this.tracer?.recordTcpProxy("remote_to_guest", frame.streamId, frame.payload);
        } catch {
          // Best-effort tracing: never interfere with proxy traffic.
        }
        this.onData?.(frame.streamId, frame.payload);
        return;
      }
      case TcpMuxMsgType.CLOSE: {
        const st = this.streams.get(frame.streamId);
        if (!st || st.closed) return;
        this.maybeNotifyOpen(frame.streamId);
        let flags: number;
        try {
          flags = decodeTcpMuxClosePayload(frame.payload).flags;
        } catch (err) {
          this.onError?.(frame.streamId, { code: 0, message: "Invalid CLOSE payload" });
          this.closeStream(frame.streamId);
          return;
        }

        if ((flags & TcpMuxCloseFlags.RST) !== 0) {
          this.closeStream(frame.streamId);
          return;
        }

        if ((flags & TcpMuxCloseFlags.FIN) !== 0) {
          st.remoteFin = true;

          if (st.localFin) {
            // We already sent FIN; stream is now fully closed.
            this.closeStream(frame.streamId, { keepQueuedClose: true });
            return;
          }

          if (!st.closeNotified) {
            st.closeNotified = true;
            this.onClose?.(frame.streamId);
          }
          return;
        }

        // Unknown flags: treat as a terminal close to avoid leaking stream state.
        this.closeStream(frame.streamId);
        return;
      }
      case TcpMuxMsgType.ERROR: {
        if (frame.streamId !== 0) {
          const st = this.streams.get(frame.streamId);
          if (!st || st.closed) return;
        }
        this.maybeNotifyOpen(frame.streamId);
        let decoded: TcpMuxError;
        try {
          decoded = decodeTcpMuxErrorPayload(frame.payload);
        } catch (err) {
          decoded = { code: 0, message: "Invalid ERROR payload" };
        }
        this.onError?.(frame.streamId, decoded);
        // v1 gateways do not send CLOSE after ERROR; treat ERROR as terminal.
        this.closeStream(frame.streamId);
        return;
      }
      case TcpMuxMsgType.PING: {
        // Keepalive/RTT probe.
        this.enqueue(
          TcpMuxMsgType.PONG,
          frame.streamId,
          encodeTcpMuxFrame(TcpMuxMsgType.PONG, frame.streamId, frame.payload),
        );
        return;
      }
      case TcpMuxMsgType.PONG: {
        return;
      }
      default: {
        this.onError?.(frame.streamId, {
          code: 0,
          message: formatOneLineUtf8(`Unknown msg_type ${frame.msgType}`, MAX_CLIENT_ERROR_MESSAGE_BYTES),
        });
      }
    }
  }

  private maybeNotifyOpen(streamId: number): void {
    const st = this.streams.get(streamId);
    if (!st || st.openedNotified) return;
    st.openedNotified = true;
    this.onOpen?.(streamId);
  }

  private closeStream(streamId: number, opts: { keepQueuedClose?: boolean } = {}): void {
    this.purgeQueuedFrames(streamId, { keepCloseFrames: opts.keepQueuedClose ?? false });
    const st = this.streams.get(streamId);
    if (!st || st.closed) return;
    st.closed = true;
    if (!st.closeNotified) {
      st.closeNotified = true;
      this.onClose?.(streamId);
    }
    this.streams.delete(streamId);
  }

  private purgeQueuedFrames(streamId: number, opts: { keepCloseFrames: boolean }): void {
    if (this.queuedHead > 0) {
      // Drop already-sent frames (they were only retained to avoid O(n) shift()).
      if (this.queuedHead >= this.queued.length) {
        this.queued = [];
      } else {
        this.queued = this.queued.slice(this.queuedHead);
      }
      this.queuedHead = 0;
    }
    if (this.queued.length === 0) return;
    const keepCloseFrames = opts.keepCloseFrames;
    const remaining: QueuedFrame[] = [];
    let remainingBytes = 0;
    for (const entry of this.queued) {
      if (entry.streamId === streamId && !(keepCloseFrames && entry.msgType === TcpMuxMsgType.CLOSE)) {
        continue;
      }
      remaining.push(entry);
      remainingBytes += entry.frame.byteLength;
    }
    this.queued = remaining;
    this.queuedHead = 0;
    this.queuedBytes = remainingBytes;
  }
}
