import { lookup } from "node:dns/promises";
import net from "node:net";
import type { Duplex } from "node:stream";

import { socketWritableLengthOrOverflow } from "./socketWritableLength.js";
import { unrefBestEffort } from "./unrefSafe.js";

import {
  decodeTcpMuxClosePayload,
  decodeTcpMuxOpenPayload,
  encodeTcpMuxClosePayload,
  encodeTcpMuxErrorPayload,
  encodeTcpMuxFrame,
  TCP_MUX_HEADER_BYTES,
  TcpMuxCloseFlags,
  TcpMuxErrorCode,
  TcpMuxFrameParser,
  TcpMuxMsgType,
  type TcpMuxFrame,
} from "../protocol/tcpMux.js";
import { validateTcpTargetPolicy, type TcpProxyUpgradePolicy } from "./tcpPolicy.js";
import { encodeWsClosePayload, encodeWsFrame } from "./wsFrame.js";
import { createGracefulDuplexCloser } from "./wsDuplexClose.js";
import { WsMessageReceiver } from "./wsMessage.js";
import {
  evaluateTcpHostPolicy,
  parseTcpHostnameEgressPolicyFromEnv,
  type TcpHostnameEgressPolicy,
} from "../security/egressPolicy.js";
import { isPublicIpAddress } from "../security/ipPolicy.js";
import type { SessionConnectionTracker } from "../session.js";
import { selectAllowedDnsAddress } from "./tcpDns.js";
import type { TcpProxyEgressMetricSink } from "./tcpEgressMetrics.js";
import {
  destroyBestEffort,
  endRequired,
  pauseBestEffort,
  removeAllListenersBestEffort,
  resumeBestEffort,
  setNoDelayRequired,
  setTimeoutRequired,
  writeCaptureErrorBestEffort,
} from "./socketSafe.js";

class TcpMuxIpPolicyDeniedError extends Error {
  override name = "TcpMuxIpPolicyDeniedError";
}

function formatTcpMuxHostPolicyRejection(
  reason: "invalid-hostname" | "ip-literal-disallowed" | "blocked-by-host-policy" | "not-allowed-by-host-policy",
): string {
  // Keep client-visible rejection strings stable and avoid leaking internal config knobs.
  switch (reason) {
    case "invalid-hostname":
      return "Invalid host";
    case "ip-literal-disallowed":
      return "IP-literal targets are not allowed";
    case "blocked-by-host-policy":
      return "Target is blocked";
    case "not-allowed-by-host-policy":
      return "Target is not allowed";
    default:
      return "Target is not allowed";
  }
}

export type TcpMuxBridgeOptions = TcpProxyUpgradePolicy &
  Readonly<{
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

export class WebSocketTcpMuxBridge {
  private readonly wsSocket: Duplex;
  private readonly opts: TcpMuxBridgeOptions;
  private readonly maxMessageBytes: number;
  private readonly hostnamePolicy: TcpHostnameEgressPolicy | null;
  private readonly wsMessages: WsMessageReceiver;
  private readonly wsCloser: ReturnType<typeof createGracefulDuplexCloser>;

  private readonly muxParser = new TcpMuxFrameParser();
  private readonly streams = new Map<number, StreamState>();

  private pausedForWsBackpressure = false;
  private closed = false;

  constructor(wsSocket: Duplex, opts: TcpMuxBridgeOptions) {
    this.wsSocket = wsSocket;
    this.opts = opts;
    this.wsCloser = createGracefulDuplexCloser(wsSocket);
    this.maxMessageBytes = opts.maxMessageBytes ?? 1024 * 1024;
    try {
      this.hostnamePolicy = parseTcpHostnameEgressPolicyFromEnv(process.env);
    } catch {
      this.hostnamePolicy = null;
    }
    this.wsMessages = new WsMessageReceiver({
      maxMessageBytes: this.maxMessageBytes,
      sendWsFrame: (opcode, payload) => this.sendWsFrame(opcode, payload),
      onMessage: (opcode, payload) => this.forwardMessage(opcode, payload),
      onClose: () => this.closeGracefully(),
      closeWithProtocolError: () => this.closeWithProtocolError(),
      closeWithMessageTooLarge: () => this.closeWithMessageTooLarge(),
    });
  }

  start(head: Buffer): void {
    if (head.length > 0) this.wsMessages.push(head);

    this.wsSocket.on("data", (data) => {
      this.wsMessages.push(data);
    });
    this.wsSocket.on("error", () => this.destroyNow());
    this.wsSocket.on("close", () => this.destroyNow());
    this.wsSocket.on("end", () => this.destroyNow());
    this.wsSocket.on("drain", () => this.onWsDrain());
  }

  private onWsDrain(): void {
    if (this.closed) return;
    if (!this.pausedForWsBackpressure) return;
    this.pausedForWsBackpressure = false;
    for (const stream of this.streams.values()) {
      resumeBestEffort(stream.socket);
    }
  }

  private pauseAllTcpReads(): void {
    if (this.pausedForWsBackpressure) return;
    this.pausedForWsBackpressure = true;
    for (const stream of this.streams.values()) {
      pauseBestEffort(stream.socket);
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
      // Keep client-visible error strings stable; do not reflect parser exception messages.
      this.sendStreamError(frame.streamId, TcpMuxErrorCode.PROTOCOL_ERROR, "Invalid OPEN payload");
      return;
    }

    const targetDecision = validateTcpTargetPolicy(target.host, target.port, this.opts);
    if (!targetDecision.ok) {
      const code = targetDecision.status === 400 ? TcpMuxErrorCode.PROTOCOL_ERROR : TcpMuxErrorCode.POLICY_DENIED;
      this.sendStreamError(frame.streamId, code, targetDecision.message);
      return;
    }

    if (!this.hostnamePolicy) {
      this.sendStreamError(frame.streamId, TcpMuxErrorCode.DIAL_FAILED, "TCP hostname policy misconfigured");
      return;
    }
    const hostDecision = evaluateTcpHostPolicy(target.host, this.hostnamePolicy);
    if (!hostDecision.allowed) {
      this.opts.metrics?.blockedByHostPolicyTotal?.inc();
      const code =
        hostDecision.reason === "invalid-hostname" ? TcpMuxErrorCode.PROTOCOL_ERROR : TcpMuxErrorCode.POLICY_DENIED;
      this.sendStreamError(frame.streamId, code, formatTcpMuxHostPolicyRejection(hostDecision.reason));
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
        let cbDone = false;
        const cbOnce = (err: Error | null, address: string, family: number) => {
          if (cbDone) return;
          cbDone = true;
          cb(err, address, family);
        };

        void (async () => {
          let addresses: { address: string; family: number }[];
          try {
            addresses = await lookup(dialHost, { all: true, verbatim: true });
          } catch (err) {
            cbOnce(err as Error, "", 4);
            return;
          }

          if (addresses.length === 0) {
            cbOnce(new Error("DNS lookup returned no addresses"), "", 4);
            return;
          }

          const chosen = selectAllowedDnsAddress(addresses, allowPrivateIps);
          if (chosen) {
            cbOnce(null, chosen.address, chosen.family ?? 4);
            return;
          }
          cbOnce(new TcpMuxIpPolicyDeniedError("All resolved IPs are blocked by IP egress policy"), "", 4);
        })().catch((err) => {
          cbOnce(err as Error, "", 4);
        });
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
    let socket: net.Socket;
    try {
      socket = createConnection({
        host: dialHost,
        port: target.port,
        allowHalfOpen: true,
        lookup: dialLookup,
      });
    } catch {
      releaseSessionSlot?.();
      this.sendStreamError(frame.streamId, TcpMuxErrorCode.DIAL_FAILED, "dial failed");
      return;
    }
    if (!setNoDelayRequired(socket, true)) {
      releaseSessionSlot?.();
      destroyBestEffort(socket);
      this.sendStreamError(frame.streamId, TcpMuxErrorCode.DIAL_FAILED, "dial failed");
      return;
    }

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
      pauseBestEffort(socket);
    }

    const connectTimeoutMs = this.opts.connectTimeoutMs ?? 10_000;
    const idleTimeoutMs = this.opts.idleTimeoutMs ?? 300_000;

    if (!setTimeoutRequired(socket, idleTimeoutMs)) {
      this.sendStreamError(frame.streamId, TcpMuxErrorCode.DIAL_FAILED, "dial failed");
      this.destroyStream(frame.streamId);
      return;
    }
    socket.on("timeout", () => {
      this.sendStreamError(stream.id, TcpMuxErrorCode.DIAL_FAILED, "TCP idle timeout");
      this.destroyStream(stream.id);
    });

    const connectTimer = setTimeout(() => {
      this.sendStreamError(stream.id, TcpMuxErrorCode.DIAL_FAILED, "TCP connect timeout");
      this.destroyStream(stream.id);
    }, connectTimeoutMs);
    unrefBestEffort(connectTimer);
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
        // Keep a stable message; avoid depending on arbitrary Error.message.
        this.sendStreamError(stream.id, TcpMuxErrorCode.POLICY_DENIED, "Target IP is not allowed by IP egress policy");
      } else {
        // Avoid reflecting raw socket error messages (which may include resolved IPs).
        this.sendStreamError(stream.id, TcpMuxErrorCode.DIAL_FAILED, "dial failed");
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

    const res = writeCaptureErrorBestEffort(stream.socket, frame.payload);
    if (res.err) {
      // Avoid reflecting raw error details.
      this.sendStreamError(stream.id, TcpMuxErrorCode.DIAL_FAILED, "dial failed");
      this.destroyStream(stream.id);
      return;
    }
    if (!res.ok) {
      stream.writePaused = true;
      return;
    }
    this.enforceTcpBackpressure(stream);
  }

  private handleClose(frame: TcpMuxFrame): void {
    const stream = this.streams.get(frame.streamId);
    if (!stream) return;

    let flags: number;
    try {
      flags = decodeTcpMuxClosePayload(frame.payload).flags;
    } catch (err) {
      // Keep client-visible error strings stable; do not reflect parser exception messages.
      this.sendStreamError(frame.streamId, TcpMuxErrorCode.PROTOCOL_ERROR, "Invalid CLOSE payload");
      return;
    }

    if ((flags & TcpMuxCloseFlags.RST) !== 0) {
      this.destroyStream(frame.streamId);
      return;
    }

    if ((flags & TcpMuxCloseFlags.FIN) !== 0) {
      stream.clientFin = true;
      if (!endRequired(stream.socket)) {
        // Avoid reflecting raw error details.
        this.sendStreamError(stream.id, TcpMuxErrorCode.DIAL_FAILED, "dial failed");
        this.destroyStream(stream.id);
      }
    }
  }

  private enqueueStreamWrite(stream: StreamState, data: Buffer): void {
    const maxStreamBufferedBytes = this.maxStreamBufferedBytes();
    stream.pendingWrites.push(data);
    stream.pendingWriteBytes += data.length;
    // Enforce combined queued bytes + socket-level buffering (writableLength), even when writes
    // are paused due to backpressure.
    const socketBuffered = socketWritableLengthOrOverflow(stream.socket, maxStreamBufferedBytes);
    if (stream.pendingWriteBytes + socketBuffered > maxStreamBufferedBytes) {
      this.sendStreamError(stream.id, TcpMuxErrorCode.STREAM_BUFFER_OVERFLOW, "stream buffered too much data");
      this.destroyStream(stream.id);
    }
  }

  private maxStreamBufferedBytes(): number {
    return this.opts.maxStreamBufferedBytes ?? 1024 * 1024;
  }

  private enforceTcpBackpressure(stream: StreamState): boolean {
    const max = this.maxStreamBufferedBytes();
    const socketBuffered = socketWritableLengthOrOverflow(stream.socket, max);
    const totalBuffered = stream.pendingWriteBytes + socketBuffered;
    if (totalBuffered <= max) return true;
    this.sendStreamError(stream.id, TcpMuxErrorCode.STREAM_BUFFER_OVERFLOW, "stream buffered too much data");
    this.destroyStream(stream.id);
    return false;
  }

  private flushStreamWrites(stream: StreamState): void {
    if (this.closed) return;
    if (!stream.connected) return;
    if (stream.writePaused) return;

    while (stream.pendingWrites.length > 0) {
      const chunk = stream.pendingWrites.shift()!;
      stream.pendingWriteBytes -= chunk.length;
      const res = writeCaptureErrorBestEffort(stream.socket, chunk);
      if (res.err) {
        // Avoid reflecting raw error details.
        this.sendStreamError(stream.id, TcpMuxErrorCode.DIAL_FAILED, "dial failed");
        this.destroyStream(stream.id);
        return;
      }
      if (!res.ok) {
        stream.writePaused = true;
        return;
      }
      if (!this.enforceTcpBackpressure(stream)) return;
    }
  }

  private destroyStream(streamId: number): void {
    const stream = this.streams.get(streamId);
    if (!stream) return;
    this.streams.delete(streamId);
    if (stream.connectTimer) clearTimeout(stream.connectTimer);
    stream.releaseSessionSlot?.();
    removeAllListenersBestEffort(stream.socket);
    destroyBestEffort(stream.socket);
  }

  private sendStreamError(streamId: number, code: TcpMuxErrorCode, message: string): void {
    this.sendMuxFrame(TcpMuxMsgType.ERROR, streamId, encodeTcpMuxErrorPayload(code, message));
  }

  private sendMuxFrame(msgType: TcpMuxMsgType, streamId: number, payload?: Buffer): void {
    this.sendWsFrame(0x2, encodeTcpMuxFrame(msgType, streamId, payload));
  }

  private sendWsFrame(opcode: number, payload: Buffer): void {
    if (this.closed) return;
    const frame = encodeWsFrame(opcode, payload);
    const res = writeCaptureErrorBestEffort(this.wsSocket, frame);
    if (res.err) {
      this.destroyNow();
      return;
    }
    if (!res.ok) {
      this.pauseAllTcpReads();
    }
  }

  private closeWithProtocolError(): void {
    // 1002 = protocol error.
    this.sendWsFrame(0x8, encodeWsClosePayload(1002));
    this.closeGracefully();
  }

  private closeWithMessageTooLarge(): void {
    // 1009 = message too big.
    this.sendWsFrame(0x8, encodeWsClosePayload(1009));
    this.closeGracefully();
  }

  private closeWithUnsupportedData(): void {
    // 1003 = unsupported data.
    this.sendWsFrame(0x8, encodeWsClosePayload(1003));
    this.closeGracefully();
  }

  private closeGracefully(): void {
    if (this.closed) return;
    this.closed = true;

    for (const id of this.streams.keys()) {
      this.destroyStream(id);
    }

    // `WsMessageReceiver` writes the close response frame before invoking `onClose()`.
    // Avoid destroying the underlying socket until pending writes have a chance to flush.
    this.wsCloser.endThenDestroy();
  }

  private destroyNow(): void {
    if (this.closed) return;
    this.closed = true;

    for (const id of this.streams.keys()) {
      this.destroyStream(id);
    }

    this.wsCloser.destroyNow();
  }
}

