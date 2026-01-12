import { createHash } from "node:crypto";
import { lookup } from "node:dns/promises";
import net from "node:net";
import type http from "node:http";
import type { Duplex } from "node:stream";

import {
  decodeTcpMuxClosePayload,
  decodeTcpMuxOpenPayload,
  encodeTcpMuxClosePayload,
  encodeTcpMuxErrorPayload,
  encodeTcpMuxFrame,
  TCP_MUX_HEADER_BYTES,
  TCP_MUX_SUBPROTOCOL,
  TcpMuxCloseFlags,
  TcpMuxErrorCode,
  TcpMuxFrameParser,
  TcpMuxMsgType,
  type TcpMuxFrame,
} from "../protocol/tcpMux.js";
import { validateTcpTargetPolicy, validateWsUpgradePolicy, type TcpProxyUpgradePolicy } from "./tcpPolicy.js";
import { evaluateTcpHostPolicy, parseTcpHostnameEgressPolicyFromEnv } from "../security/egressPolicy.js";
import { isPublicIpAddress } from "../security/ipPolicy.js";
import type { SessionConnectionTracker } from "../session.js";

const WS_GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

type TcpProxyEgressMetricSink = Readonly<{
  blockedByHostPolicyTotal?: { inc: () => void };
  blockedByIpPolicyTotal?: { inc: () => void };
}>;

class TcpMuxIpPolicyDeniedError extends Error {
  override name = "TcpMuxIpPolicyDeniedError";
}

export type TcpMuxUpgradeOptions = TcpProxyUpgradePolicy &
  Readonly<{
    /**
     * Expected request pathname for this upgrade. Defaults to `/tcp-mux`.
     *
     * The gateway may be deployed under a base-path prefix (e.g. `/aero/tcp-mux`).
     * In those cases the HTTP server can route upgrades by pathname and then
     * pass that pathname here for an additional defense-in-depth check.
     */
    expectedPathname?: string;
    allowPrivateIps?: boolean;
    maxStreams?: number;
    maxStreamBufferedBytes?: number;
    maxFramePayloadBytes?: number;
    maxMessageBytes?: number;
    connectTimeoutMs?: number;
    idleTimeoutMs?: number;
    sessionId?: string;
    sessionConnections?: SessionConnectionTracker;
    createConnection?: typeof net.createConnection;
    metrics?: TcpProxyEgressMetricSink;
  }>;

export function handleTcpMuxUpgrade(
  req: http.IncomingMessage,
  socket: Duplex,
  head: Buffer,
  opts: TcpMuxUpgradeOptions = {},
): void {
  const upgradeDecision = validateWsUpgradePolicy(req, opts);
  if (!upgradeDecision.ok) {
    respondHttp(socket, upgradeDecision.status, upgradeDecision.message);
    return;
  }

  let url: URL;
  try {
    url = new URL(req.url ?? "", "http://localhost");
  } catch {
    respondHttp(socket, 400, "Invalid request");
    return;
  }
  const expectedPathname = opts.expectedPathname ?? "/tcp-mux";
  if (url.pathname !== expectedPathname) {
    respondHttp(socket, 404, "Not Found");
    return;
  }

  const protocolHeader = req.headers["sec-websocket-protocol"];
  const offered = typeof protocolHeader === "string" ? protocolHeader : "";
  const protocols = offered
    .split(",")
    .map((p: string) => p.trim())
    .filter((p: string) => p.length > 0);
  if (!protocols.includes(TCP_MUX_SUBPROTOCOL)) {
    respondHttp(socket, 400, `Missing required subprotocol: ${TCP_MUX_SUBPROTOCOL}`);
    return;
  }

  const key = req.headers["sec-websocket-key"];
  if (typeof key !== "string" || key === "") {
    respondHttp(socket, 400, "Missing required header: Sec-WebSocket-Key");
    return;
  }

  const accept = createHash("sha1").update(key + WS_GUID).digest("base64");
  socket.write(
    [
      "HTTP/1.1 101 Switching Protocols",
      "Upgrade: websocket",
      "Connection: Upgrade",
      `Sec-WebSocket-Accept: ${accept}`,
      `Sec-WebSocket-Protocol: ${TCP_MUX_SUBPROTOCOL}`,
      "\r\n",
    ].join("\r\n"),
  );

  if ("setNoDelay" in socket && typeof socket.setNoDelay === "function") {
    socket.setNoDelay(true);
  }

  const bridge = new WebSocketTcpMuxBridge(socket, opts);
  bridge.start(head);
}

function respondHttp(socket: Duplex, status: number, message: string): void {
  const body = `${message}\n`;
  socket.end(
    [
      `HTTP/1.1 ${status} ${httpStatusText(status)}`,
      "Content-Type: text/plain; charset=utf-8",
      `Content-Length: ${Buffer.byteLength(body)}`,
      "Connection: close",
      "\r\n",
      body,
    ].join("\r\n"),
  );
}

function httpStatusText(status: number): string {
  switch (status) {
    case 400:
      return "Bad Request";
    case 403:
      return "Forbidden";
    case 404:
      return "Not Found";
    default:
      return "Error";
  }
}

type StreamState = {
  id: number;
  socket: net.Socket;
  connected: boolean;
  clientFin: boolean;
  serverFin: boolean;
  pendingWrites: Buffer[];
  pendingWriteBytes: number;
  writePaused: boolean;
  connectTimer?: ReturnType<typeof setTimeout>;
  releaseSessionSlot?: () => void;
};

class WebSocketTcpMuxBridge {
  private readonly wsSocket: Duplex;
  private readonly opts: TcpMuxUpgradeOptions;
  private readonly maxMessageBytes: number;

  private wsBuffer: Buffer = Buffer.alloc(0);

  private fragmentedOpcode: number | null = null;
  private fragmentedChunks: Buffer[] = [];
  private fragmentedBytes = 0;

  private readonly muxParser = new TcpMuxFrameParser();
  private readonly streams = new Map<number, StreamState>();

  private pausedForWsBackpressure = false;
  private closed = false;

  constructor(wsSocket: Duplex, opts: TcpMuxUpgradeOptions) {
    this.wsSocket = wsSocket;
    this.opts = opts;
    this.maxMessageBytes = opts.maxMessageBytes ?? 1024 * 1024;
  }

  start(head: Buffer): void {
    if (head.length > 0) {
      this.wsBuffer = this.wsBuffer.length === 0 ? head : Buffer.concat([this.wsBuffer, head]);
    }

    this.wsSocket.on("data", (data) => {
      this.wsBuffer = this.wsBuffer.length === 0 ? data : Buffer.concat([this.wsBuffer, data]);
      this.drainWebSocketFrames();
    });
    this.wsSocket.on("error", () => this.close());
    this.wsSocket.on("close", () => this.close());
    this.wsSocket.on("end", () => this.close());
    this.wsSocket.on("drain", () => this.onWsDrain());

    this.drainWebSocketFrames();
  }

  private onWsDrain(): void {
    if (this.closed) return;
    if (!this.pausedForWsBackpressure) return;
    this.pausedForWsBackpressure = false;
    for (const stream of this.streams.values()) {
      stream.socket.resume();
    }
  }

  private pauseAllTcpReads(): void {
    if (this.pausedForWsBackpressure) return;
    this.pausedForWsBackpressure = true;
    for (const stream of this.streams.values()) {
      stream.socket.pause();
    }
  }

  private drainWebSocketFrames(): void {
    while (!this.closed) {
      const parsed = tryReadFrame(this.wsBuffer, this.maxMessageBytes);
      if (!parsed) return;
      this.wsBuffer = parsed.remaining;
      this.handleWsFrame(parsed.frame);
    }
  }

  private handleWsFrame(frame: ParsedFrame): void {
    switch (frame.opcode) {
      case 0x0: {
        // Continuation
        if (this.fragmentedOpcode === null) {
          this.closeWithProtocolError();
          return;
        }
        this.fragmentedChunks.push(frame.payload);
        this.fragmentedBytes += frame.payload.length;
        if (this.fragmentedBytes > this.maxMessageBytes) {
          this.closeWithMessageTooLarge();
          return;
        }
        if (frame.fin) {
          const payload = Buffer.concat(this.fragmentedChunks);
          const opcode = this.fragmentedOpcode;
          this.fragmentedOpcode = null;
          this.fragmentedChunks = [];
          this.fragmentedBytes = 0;
          this.forwardMessage(opcode, payload);
        }
        return;
      }
      case 0x1:
      case 0x2: {
        // Text / Binary
        if (this.fragmentedOpcode !== null) {
          this.closeWithProtocolError();
          return;
        }
        if (frame.fin) {
          this.forwardMessage(frame.opcode, frame.payload);
          return;
        }
        this.fragmentedOpcode = frame.opcode;
        this.fragmentedChunks = [frame.payload];
        this.fragmentedBytes = frame.payload.length;
        if (this.fragmentedBytes > this.maxMessageBytes) {
          this.closeWithMessageTooLarge();
          return;
        }
        return;
      }
      case 0x8: {
        // Close
        this.sendWsFrame(0x8, frame.payload);
        this.close();
        return;
      }
      case 0x9: {
        // Ping
        this.sendWsFrame(0xA, frame.payload);
        return;
      }
      case 0xA: {
        // Pong
        return;
      }
      default: {
        this.closeWithProtocolError();
      }
    }
  }

  private forwardMessage(opcode: number, payload: Buffer): void {
    // /tcp-mux is a binary protocol; reject text messages to avoid accidental
    // corruption due to UTF-8 re-encoding.
    if (opcode !== 0x2) {
      this.closeWithUnsupportedData();
      return;
    }

    for (const frame of this.muxParser.push(payload)) {
      this.handleMuxFrame(frame);
    }

    const maxFramePayloadBytes = this.opts.maxFramePayloadBytes ?? 16 * 1024 * 1024;
    const pending = this.muxParser.peekHeader();
    if (pending && pending.payloadLength > maxFramePayloadBytes) {
      this.closeWithProtocolError();
      return;
    }
    if (this.muxParser.pendingBytes() > TCP_MUX_HEADER_BYTES + maxFramePayloadBytes) {
      this.closeWithProtocolError();
    }
  }

  private handleMuxFrame(frame: TcpMuxFrame): void {
    const maxFramePayloadBytes = this.opts.maxFramePayloadBytes ?? 16 * 1024 * 1024;
    if (frame.payload.length > maxFramePayloadBytes) {
      this.closeWithProtocolError();
      return;
    }

    switch (frame.msgType) {
      case TcpMuxMsgType.OPEN: {
        this.handleOpen(frame);
        return;
      }
      case TcpMuxMsgType.DATA: {
        this.handleData(frame);
        return;
      }
      case TcpMuxMsgType.CLOSE: {
        this.handleClose(frame);
        return;
      }
      case TcpMuxMsgType.ERROR: {
        // Not used by v1 clients; ignore.
        return;
      }
      case TcpMuxMsgType.PING: {
        this.sendMuxFrame(TcpMuxMsgType.PONG, frame.streamId, frame.payload);
        return;
      }
      case TcpMuxMsgType.PONG: {
        return;
      }
      default: {
        this.sendStreamError(frame.streamId, TcpMuxErrorCode.PROTOCOL_ERROR, `Unknown msg_type ${frame.msgType}`);
      }
    }
  }

  private handleOpen(frame: TcpMuxFrame): void {
    if (frame.streamId === 0) {
      this.sendStreamError(frame.streamId, TcpMuxErrorCode.PROTOCOL_ERROR, "stream_id=0 is reserved");
      return;
    }
    if (this.streams.has(frame.streamId)) {
      this.sendStreamError(frame.streamId, TcpMuxErrorCode.PROTOCOL_ERROR, "stream_id already exists");
      return;
    }

    const maxStreams = this.opts.maxStreams ?? 1024;
    if (this.streams.size >= maxStreams) {
      this.sendStreamError(frame.streamId, TcpMuxErrorCode.STREAM_LIMIT_EXCEEDED, "max streams exceeded");
      return;
    }

    let target: { host: string; port: number };
    try {
      target = decodeTcpMuxOpenPayload(frame.payload);
    } catch (err) {
      this.sendStreamError(frame.streamId, TcpMuxErrorCode.PROTOCOL_ERROR, (err as Error).message);
      return;
    }

    const targetDecision = validateTcpTargetPolicy(target.host, target.port, this.opts);
    if (!targetDecision.ok) {
      const code = targetDecision.status === 400 ? TcpMuxErrorCode.PROTOCOL_ERROR : TcpMuxErrorCode.POLICY_DENIED;
      this.sendStreamError(frame.streamId, code, targetDecision.message);
      return;
    }

    let hostDecision: ReturnType<typeof evaluateTcpHostPolicy>;
    try {
      const hostPolicy = parseTcpHostnameEgressPolicyFromEnv(process.env);
      hostDecision = evaluateTcpHostPolicy(target.host, hostPolicy);
    } catch {
      this.sendStreamError(frame.streamId, TcpMuxErrorCode.DIAL_FAILED, "TCP hostname policy misconfigured");
      return;
    }
    if (!hostDecision.allowed) {
      this.opts.metrics?.blockedByHostPolicyTotal?.inc();
      const code =
        hostDecision.reason === "invalid-hostname" ? TcpMuxErrorCode.PROTOCOL_ERROR : TcpMuxErrorCode.POLICY_DENIED;
      this.sendStreamError(frame.streamId, code, `${hostDecision.reason}: ${hostDecision.message}`);
      return;
    }

    const allowPrivateIps = this.opts.allowPrivateIps ?? false;

    let dialHost = "";
    let dialLookup:
      | ((hostname: string, options: unknown, cb: (err: Error | null, address: string, family: number) => void) => void)
      | undefined;

    if (hostDecision.target.kind === "ip") {
      if (!allowPrivateIps && !isPublicIpAddress(hostDecision.target.ip)) {
        this.opts.metrics?.blockedByIpPolicyTotal?.inc();
        this.sendStreamError(frame.streamId, TcpMuxErrorCode.POLICY_DENIED, "Target IP is not allowed by IP egress policy");
        return;
      }
      dialHost = hostDecision.target.ip;
    } else {
      dialHost = hostDecision.target.hostname;
      dialLookup = (_hostname, _options, cb) => {
        void (async () => {
          let addresses: { address: string; family: number }[];
          try {
            addresses = await lookup(dialHost, { all: true, verbatim: true });
          } catch (err) {
            cb(err as Error, "", 4);
            return;
          }

          if (addresses.length === 0) {
            cb(new Error("DNS lookup returned no addresses"), "", 4);
            return;
          }

          if (allowPrivateIps) {
            const { address, family } = addresses[0]!;
            cb(null, address, family);
            return;
          }

          for (const { address, family } of addresses) {
            if (isPublicIpAddress(address)) {
              cb(null, address, family);
              return;
            }
          }

          cb(new TcpMuxIpPolicyDeniedError("All resolved IPs are blocked by IP egress policy"), "", 4);
        })();
      };
    }

    let releaseSessionSlot: (() => void) | undefined;
    if (this.opts.sessionId && this.opts.sessionConnections) {
      if (!this.opts.sessionConnections.tryAcquire(this.opts.sessionId)) {
        this.sendStreamError(frame.streamId, TcpMuxErrorCode.STREAM_LIMIT_EXCEEDED, "session max connections exceeded");
        return;
      }
      let released = false;
      releaseSessionSlot = () => {
        if (released) return;
        released = true;
        this.opts.sessionConnections!.release(this.opts.sessionId!);
      };
    }

    const createConnection = this.opts.createConnection ?? net.createConnection;
    const socket = createConnection({
      host: dialHost,
      port: target.port,
      allowHalfOpen: true,
      lookup: dialLookup,
    });
    socket.setNoDelay(true);

    const stream: StreamState = {
      id: frame.streamId,
      socket,
      connected: false,
      clientFin: false,
      serverFin: false,
      pendingWrites: [],
      pendingWriteBytes: 0,
      writePaused: false,
      releaseSessionSlot,
    };
    this.streams.set(frame.streamId, stream);
    if (this.pausedForWsBackpressure) {
      socket.pause();
    }

    const connectTimeoutMs = this.opts.connectTimeoutMs ?? 10_000;
    const idleTimeoutMs = this.opts.idleTimeoutMs ?? 300_000;

    socket.setTimeout(idleTimeoutMs);
    socket.on("timeout", () => {
      this.sendStreamError(stream.id, TcpMuxErrorCode.DIAL_FAILED, "TCP idle timeout");
      this.destroyStream(stream.id);
    });

    const connectTimer = setTimeout(() => {
      this.sendStreamError(stream.id, TcpMuxErrorCode.DIAL_FAILED, "TCP connect timeout");
      this.destroyStream(stream.id);
    }, connectTimeoutMs);
    connectTimer.unref?.();
    stream.connectTimer = connectTimer;

    socket.on("connect", () => {
      if (stream.connectTimer) clearTimeout(stream.connectTimer);
      stream.connectTimer = undefined;
      stream.connected = true;
      this.flushStreamWrites(stream);
    });

    socket.on("data", (chunk) => {
      this.sendMuxFrame(TcpMuxMsgType.DATA, stream.id, chunk);
    });

    socket.on("drain", () => {
      stream.writePaused = false;
      this.flushStreamWrites(stream);
    });

    socket.on("end", () => {
      if (stream.serverFin) return;
      stream.serverFin = true;
      this.sendMuxFrame(TcpMuxMsgType.CLOSE, stream.id, encodeTcpMuxClosePayload(TcpMuxCloseFlags.FIN));
    });

    socket.on("error", (err) => {
      if (stream.connectTimer) clearTimeout(stream.connectTimer);
      stream.connectTimer = undefined;
      if (err instanceof TcpMuxIpPolicyDeniedError) {
        this.opts.metrics?.blockedByIpPolicyTotal?.inc();
        this.sendStreamError(stream.id, TcpMuxErrorCode.POLICY_DENIED, err.message);
      } else {
        this.sendStreamError(stream.id, TcpMuxErrorCode.DIAL_FAILED, err.message);
      }
      this.destroyStream(stream.id);
    });

    socket.on("close", () => {
      if (stream.connectTimer) clearTimeout(stream.connectTimer);
      stream.connectTimer = undefined;
      const existing = this.streams.get(stream.id);
      if (!existing) return;
      this.streams.delete(stream.id);
      existing.releaseSessionSlot?.();
    });
  }

  private handleData(frame: TcpMuxFrame): void {
    const stream = this.streams.get(frame.streamId);
    if (!stream) {
      this.sendStreamError(frame.streamId, TcpMuxErrorCode.UNKNOWN_STREAM, "unknown stream");
      return;
    }
    if (stream.clientFin) {
      this.sendStreamError(frame.streamId, TcpMuxErrorCode.PROTOCOL_ERROR, "stream is half-closed (client FIN)");
      return;
    }

    if (!stream.connected || stream.writePaused) {
      this.enqueueStreamWrite(stream, frame.payload);
      return;
    }

    const ok = stream.socket.write(frame.payload);
    if (!ok) {
      stream.writePaused = true;
    }
  }

  private handleClose(frame: TcpMuxFrame): void {
    const stream = this.streams.get(frame.streamId);
    if (!stream) return;

    let flags: number;
    try {
      flags = decodeTcpMuxClosePayload(frame.payload).flags;
    } catch (err) {
      this.sendStreamError(frame.streamId, TcpMuxErrorCode.PROTOCOL_ERROR, (err as Error).message);
      return;
    }

    if ((flags & TcpMuxCloseFlags.RST) !== 0) {
      this.destroyStream(frame.streamId);
      return;
    }

    if ((flags & TcpMuxCloseFlags.FIN) !== 0) {
      stream.clientFin = true;
      stream.socket.end();
    }
  }

  private enqueueStreamWrite(stream: StreamState, data: Buffer): void {
    const maxStreamBufferedBytes = this.opts.maxStreamBufferedBytes ?? 1024 * 1024;
    stream.pendingWrites.push(data);
    stream.pendingWriteBytes += data.length;
    if (stream.pendingWriteBytes > maxStreamBufferedBytes) {
      this.sendStreamError(stream.id, TcpMuxErrorCode.STREAM_BUFFER_OVERFLOW, "stream buffered too much data");
      this.destroyStream(stream.id);
    }
  }

  private flushStreamWrites(stream: StreamState): void {
    if (this.closed) return;
    if (!stream.connected) return;
    if (stream.writePaused) return;

    while (stream.pendingWrites.length > 0) {
      const chunk = stream.pendingWrites.shift()!;
      stream.pendingWriteBytes -= chunk.length;
      const ok = stream.socket.write(chunk);
      if (!ok) {
        stream.writePaused = true;
        return;
      }
    }
  }

  private destroyStream(streamId: number): void {
    const stream = this.streams.get(streamId);
    if (!stream) return;
    this.streams.delete(streamId);
    if (stream.connectTimer) clearTimeout(stream.connectTimer);
    stream.releaseSessionSlot?.();
    stream.socket.removeAllListeners();
    stream.socket.destroy();
  }

  private sendStreamError(streamId: number, code: TcpMuxErrorCode, message: string): void {
    this.sendMuxFrame(TcpMuxMsgType.ERROR, streamId, encodeTcpMuxErrorPayload(code, message));
  }

  private sendMuxFrame(msgType: TcpMuxMsgType, streamId: number, payload?: Buffer): void {
    this.sendWsFrame(0x2, encodeTcpMuxFrame(msgType, streamId, payload));
  }

  private sendWsFrame(opcode: number, payload: Buffer): void {
    if (this.closed) return;
    const frame = encodeFrame(opcode, payload);
    const ok = this.wsSocket.write(frame);
    if (!ok) {
      this.pauseAllTcpReads();
    }
  }

  private closeWithProtocolError(): void {
    // 1002 = protocol error.
    this.sendWsFrame(0x8, Buffer.from([0x03, 0xea]));
    this.close();
  }

  private closeWithMessageTooLarge(): void {
    // 1009 = message too big.
    this.sendWsFrame(0x8, Buffer.from([0x03, 0xf1]));
    this.close();
  }

  private closeWithUnsupportedData(): void {
    // 1003 = unsupported data.
    this.sendWsFrame(0x8, Buffer.from([0x03, 0xeb]));
    this.close();
  }

  private close(): void {
    if (this.closed) return;
    this.closed = true;

    for (const id of this.streams.keys()) {
      this.destroyStream(id);
    }

    this.wsSocket.destroy();
  }
}

type ParsedFrame = {
  opcode: number;
  fin: boolean;
  payload: Buffer;
};

type TryReadFrameResult = { frame: ParsedFrame; remaining: Buffer };

function tryReadFrame(buffer: Buffer, maxPayloadBytes: number): TryReadFrameResult | null {
  if (buffer.length < 2) return null;

  const first = buffer[0];
  const second = buffer[1];

  const fin = (first & 0x80) !== 0;
  const opcode = first & 0x0f;

  const masked = (second & 0x80) !== 0;
  let length = second & 0x7f;
  let offset = 2;

  if (length === 126) {
    if (buffer.length < offset + 2) return null;
    length = buffer.readUInt16BE(offset);
    offset += 2;
  } else if (length === 127) {
    if (buffer.length < offset + 8) return null;
    const hi = buffer.readUInt32BE(offset);
    const lo = buffer.readUInt32BE(offset + 4);
    offset += 8;
    const combined = hi * 2 ** 32 + lo;
    if (!Number.isSafeInteger(combined)) {
      // Too large for a JS buffer anyway; treat as protocol error.
      return { frame: { fin: true, opcode: 0x8, payload: Buffer.alloc(0) }, remaining: Buffer.alloc(0) };
    }
    length = combined;
  }

  if (length > maxPayloadBytes) {
    // Close immediately without buffering untrusted payloads.
    return { frame: { fin: true, opcode: 0x8, payload: Buffer.from([0x03, 0xf1]) }, remaining: Buffer.alloc(0) };
  }

  let maskKey: Buffer | null = null;
  if (masked) {
    if (buffer.length < offset + 4) return null;
    maskKey = buffer.subarray(offset, offset + 4);
    offset += 4;
  }

  if (buffer.length < offset + length) return null;
  let payload = buffer.subarray(offset, offset + length);
  const remaining = buffer.subarray(offset + length);

  if (masked) {
    payload = unmask(payload, maskKey!);
  }

  // If we consumed the entire buffer, avoid keeping a reference to the backing allocation
  // via an empty subarray view.
  const remainingTrimmed = remaining.length === 0 ? Buffer.alloc(0) : remaining;
  return { frame: { fin, opcode, payload }, remaining: remainingTrimmed };
}

function unmask(payload: Buffer, key: Buffer): Buffer {
  const out = Buffer.allocUnsafe(payload.length);
  for (let i = 0; i < payload.length; i++) {
    out[i] = payload[i] ^ key[i % 4];
  }
  return out;
}

function encodeFrame(opcode: number, payload: Buffer): Buffer {
  const finOpcode = 0x80 | (opcode & 0x0f);
  const length = payload.length;

  if (length < 126) {
    const header = Buffer.alloc(2);
    header[0] = finOpcode;
    header[1] = length;
    return Buffer.concat([header, payload]);
  }

  if (length < 65536) {
    const header = Buffer.alloc(4);
    header[0] = finOpcode;
    header[1] = 126;
    header.writeUInt16BE(length, 2);
    return Buffer.concat([header, payload]);
  }

  const header = Buffer.alloc(10);
  header[0] = finOpcode;
  header[1] = 127;
  header.writeUInt32BE(0, 2);
  header.writeUInt32BE(length, 6);
  return Buffer.concat([header, payload]);
}
