// Browser-side multiplexed TCP proxy client.
//
// This is used by the in-browser network stack (NT-STACK) to open outbound TCP
// connections via a WebSocket relay (`tools/net-proxy-server`).

export enum TcpProxyFrameType {
  OPEN = 1,
  DATA = 2,
  CLOSE = 3,
  ERROR = 4,
}

export interface TcpProxyError {
  code: number;
  message: string;
}

export interface TcpProxyOptions {
  /**
   * Auth token to send as `?token=...` in the WebSocket URL.
   *
   * Browser WebSockets cannot set arbitrary headers, so the token must be in
   * the URL query string (or encoded in a cookie controlled by the relay).
   */
  authToken?: string;

  /**
   * Maximum number of bytes allowed to be queued in JS before we start
   * rejecting writes. This prevents unbounded memory growth if the WebSocket
   * is slow or disconnected.
   */
  maxQueuedBytes?: number;

  /**
   * If `WebSocket.bufferedAmount` exceeds this, we stop flushing the queue
   * until it drains again.
   */
  maxBufferedAmount?: number;

  /**
   * DATA frame payload chunk size. Smaller chunks improve fairness across
   * multiplexed streams.
   */
  chunkSize?: number;
}

function ipv4StringToBytes(ip: string): Uint8Array {
  const parts = ip.split(".");
  if (parts.length !== 4) throw new Error(`Invalid IPv4 address: ${ip}`);
  const out = new Uint8Array(4);
  for (let i = 0; i < 4; i++) {
    const n = Number(parts[i]);
    if (!Number.isInteger(n) || n < 0 || n > 255) throw new Error(`Invalid IPv4 address: ${ip}`);
    out[i] = n;
  }
  return out;
}

function encodeHeader(type: number, connectionId: number, payloadLen: number): Uint8Array {
  const buf = new Uint8Array(5 + payloadLen);
  const dv = new DataView(buf.buffer);
  dv.setUint8(0, type);
  dv.setUint32(1, connectionId >>> 0, false);
  return buf;
}

function encodeOpenRequest(connectionId: number, dstIpV4: Uint8Array, dstPort: number): Uint8Array {
  const buf = encodeHeader(TcpProxyFrameType.OPEN, connectionId, 1 + 4 + 2);
  buf[5] = 4;
  buf.set(dstIpV4, 6);
  new DataView(buf.buffer).setUint16(10, dstPort, false);
  return buf;
}

function encodeData(connectionId: number, payload: Uint8Array): Uint8Array {
  const buf = encodeHeader(TcpProxyFrameType.DATA, connectionId, payload.byteLength);
  buf.set(payload, 5);
  return buf;
}

function encodeClose(connectionId: number): Uint8Array {
  return encodeHeader(TcpProxyFrameType.CLOSE, connectionId, 0);
}

function decodeFrame(data: ArrayBuffer): { type: number; connectionId: number; payload: Uint8Array } {
  const u8 = new Uint8Array(data);
  if (u8.byteLength < 5) throw new Error("Frame too short");
  const dv = new DataView(u8.buffer);
  const type = dv.getUint8(0);
  const connectionId = dv.getUint32(1, false);
  return { type, connectionId, payload: u8.subarray(5) };
}

function decodeErrorPayload(payload: Uint8Array): TcpProxyError {
  if (payload.byteLength < 4) throw new Error("Invalid ERROR payload");
  const dv = new DataView(payload.buffer, payload.byteOffset, payload.byteLength);
  const code = dv.getUint16(0, false);
  const msgLen = dv.getUint16(2, false);
  if (payload.byteLength !== 4 + msgLen) throw new Error("Invalid ERROR msg_len");
  const msgBytes = payload.subarray(4, 4 + msgLen);
  const message = new TextDecoder().decode(msgBytes);
  return { code, message };
}

type ConnState = "opening" | "open" | "closing" | "closed";

export class TcpProxy {
  on_open?: (connectionId: number) => void;
  on_data?: (connectionId: number, data: Uint8Array) => void;
  on_close?: (connectionId: number) => void;
  on_error?: (connectionId: number, error: TcpProxyError) => void;

  private ws: WebSocket;
  private nextId = 1;
  private conns = new Map<number, ConnState>();

  private queued: Uint8Array[] = [];
  private queuedBytes = 0;
  private flushScheduled = false;

  private maxQueuedBytes: number;
  private maxBufferedAmount: number;
  private chunkSize: number;

  constructor(url: string, opts: TcpProxyOptions = {}) {
    const maxQueuedBytes = opts.maxQueuedBytes ?? 4 * 1024 * 1024;
    const maxBufferedAmount = opts.maxBufferedAmount ?? 8 * 1024 * 1024;
    const chunkSize = opts.chunkSize ?? 16 * 1024;

    this.maxQueuedBytes = maxQueuedBytes;
    this.maxBufferedAmount = maxBufferedAmount;
    this.chunkSize = chunkSize;

    const token = opts.authToken;
    const fullUrl = token ? this.appendQueryParam(url, "token", token) : url;

    this.ws = new WebSocket(fullUrl);
    this.ws.binaryType = "arraybuffer";

    this.ws.onopen = () => {
      this.scheduleFlush();
    };
    this.ws.onmessage = (ev) => {
      if (!(ev.data instanceof ArrayBuffer)) return;
      this.handleFrame(ev.data);
    };
    this.ws.onclose = () => {
      // Fail all in-flight connections on session close.
      for (const [id, state] of this.conns) {
        if (state === "closed") continue;
        this.conns.set(id, "closed");
        this.on_error?.(id, { code: 0, message: "Proxy session closed" });
        this.on_close?.(id);
      }
      this.conns.clear();
    };
    this.ws.onerror = () => {
      // Browser WebSocket errors do not expose details; treat as session-level.
      this.on_error?.(0, { code: 0, message: "WebSocket error" });
    };
  }

  connect(dst_ip: string | Uint8Array, dst_port: number): number {
    const id = this.nextId++;
    const ipBytes = typeof dst_ip === "string" ? ipv4StringToBytes(dst_ip) : dst_ip;
    if (ipBytes.byteLength !== 4) throw new Error("Only IPv4 destinations are supported");
    this.conns.set(id, "opening");
    this.enqueue(encodeOpenRequest(id, ipBytes, dst_port));
    return id;
  }

  send(connectionId: number, bytes: Uint8Array): boolean {
    const state = this.conns.get(connectionId);
    if (!state || state === "closing" || state === "closed") return false;

    for (let off = 0; off < bytes.byteLength; off += this.chunkSize) {
      const chunk = bytes.subarray(off, Math.min(bytes.byteLength, off + this.chunkSize));
      this.enqueue(encodeData(connectionId, chunk));
    }
    return true;
  }

  close(connectionId: number): void {
    const state = this.conns.get(connectionId);
    if (!state || state === "closed") return;
    this.conns.set(connectionId, "closing");
    this.enqueue(encodeClose(connectionId));
  }

  shutdown(): void {
    this.ws.close();
  }

  private appendQueryParam(url: string, key: string, value: string): string {
    const u = new URL(url);
    u.searchParams.set(key, value);
    return u.toString();
  }

  private enqueue(frame: Uint8Array): void {
    if (this.queuedBytes + frame.byteLength > this.maxQueuedBytes) {
      this.on_error?.(0, { code: 0, message: "TcpProxy send queue overflow" });
      return;
    }
    this.queued.push(frame);
    this.queuedBytes += frame.byteLength;
    this.scheduleFlush();
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
    if (this.ws.readyState !== WebSocket.OPEN) return;
    while (this.queued.length > 0) {
      if (this.ws.bufferedAmount > this.maxBufferedAmount) {
        // Let the browser drain the socket; we'll try again on the next tick.
        this.scheduleFlush();
        return;
      }
      const frame = this.queued.shift()!;
      this.queuedBytes -= frame.byteLength;
      this.ws.send(frame);
    }
  }

  private handleFrame(data: ArrayBuffer): void {
    let decoded;
    try {
      decoded = decodeFrame(data);
    } catch (e) {
      this.on_error?.(0, { code: 0, message: String((e as Error).message ?? e) });
      return;
    }

    const { type, connectionId, payload } = decoded;
    if (type === TcpProxyFrameType.OPEN) {
      // OPEN ack has an empty payload.
      const st = this.conns.get(connectionId);
      if (st) this.conns.set(connectionId, "open");
      this.on_open?.(connectionId);
      return;
    }

    if (type === TcpProxyFrameType.DATA) {
      this.on_data?.(connectionId, payload);
      return;
    }

    if (type === TcpProxyFrameType.CLOSE) {
      this.conns.set(connectionId, "closed");
      this.on_close?.(connectionId);
      this.conns.delete(connectionId);
      return;
    }

    if (type === TcpProxyFrameType.ERROR) {
      let err: TcpProxyError;
      try {
        err = decodeErrorPayload(payload);
      } catch (e) {
        err = { code: 0, message: String((e as Error).message ?? e) };
      }
      this.on_error?.(connectionId, err);
      return;
    }

    this.on_error?.(connectionId, { code: 0, message: `Unknown frame type ${type}` });
  }
}

