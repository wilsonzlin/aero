import net from "node:net";
import { createWebSocketStream, type WebSocket } from "ws";

import type { ProxyConfig } from "./config";
import type { ProxyServerMetrics } from "./metrics";
import { formatError, log } from "./logger";
import { resolveAndAuthorizeTarget } from "./security";
import { normalizeTargetHostForPolicy } from "./targetQuery";
import { wsCloseSafe } from "./wsClose";
import {
  TCP_MUX_HEADER_BYTES,
  TCP_MUX_SUBPROTOCOL,
  TcpMuxCloseFlags,
  TcpMuxErrorCode,
  TcpMuxFrameParser,
  TcpMuxMsgType,
  decodeTcpMuxClosePayload,
  decodeTcpMuxOpenPayload,
  encodeTcpMuxClosePayload,
  encodeTcpMuxErrorPayload,
  encodeTcpMuxFrame,
  type TcpMuxFrame
} from "./tcpMuxProtocol";

type TcpMuxStreamState = {
  id: number;
  host: string;
  port: number;
  socket: net.Socket | null;
  connected: boolean;
  clientFin: boolean;
  clientFinSent: boolean;
  serverFin: boolean;
  pendingWrites: Buffer[];
  pendingWriteBytes: number;
  writePaused: boolean;
  connectTimer: NodeJS.Timeout | null;
};

export function handleTcpMuxRelay(
  ws: WebSocket,
  connId: number,
  clientAddress: string | null,
  config: ProxyConfig,
  metrics: ProxyServerMetrics
): void {
  if (ws.protocol !== TCP_MUX_SUBPROTOCOL) {
    metrics.incConnectionError("denied");
    wsCloseSafe(ws, 1002, `Missing required subprotocol: ${TCP_MUX_SUBPROTOCOL}`);
    return;
  }

  metrics.connectionActiveInc("tcp_mux");

  const wsStream = createWebSocketStream(ws, { highWaterMark: config.wsStreamHighWaterMarkBytes });

  const muxParser = new TcpMuxFrameParser(config.tcpMuxMaxFramePayloadBytes);
  const streams = new Map<number, TcpMuxStreamState>();
  const usedStreamIds = new Set<number>();

  let bytesIn = 0;
  let bytesOut = 0;
  let pausedForWsBackpressure = false;
  let closed = false;

  const pauseAllTcpReads = () => {
    if (pausedForWsBackpressure) return;
    pausedForWsBackpressure = true;
    for (const stream of streams.values()) {
      stream.socket?.pause();
    }
  };

  const resumeAllTcpReads = () => {
    if (!pausedForWsBackpressure) return;
    pausedForWsBackpressure = false;
    for (const stream of streams.values()) {
      stream.socket?.resume();
    }
  };

  const destroyStream = (streamId: number) => {
    const stream = streams.get(streamId);
    if (!stream) return;
    streams.delete(streamId);
    if (stream.connectTimer) {
      clearTimeout(stream.connectTimer);
    }
    if (stream.socket) {
      metrics.tcpMuxStreamsActiveDec();
      stream.socket.removeAllListeners();
      stream.socket.destroy();
    }
  };

  const closeAll = (why: string, wsCode: number, wsReason: string) => {
    if (closed) return;
    closed = true;
    metrics.connectionActiveDec("tcp_mux");

    for (const streamId of [...streams.keys()]) {
      destroyStream(streamId);
    }

    if (ws.readyState === ws.OPEN) {
      wsCloseSafe(ws, wsCode, wsReason);
    }

    log("info", "conn_close", {
      connId,
      proto: "tcp-mux",
      why,
      bytesIn,
      bytesOut,
      clientAddress,
      wsCode,
      wsReason
    });
  };

  const sendMuxFrame = (msgType: TcpMuxMsgType, streamId: number, payload?: Buffer) => {
    if (closed) return;
    if (ws.readyState !== ws.OPEN) return;
    const frame = encodeTcpMuxFrame(msgType, streamId, payload);
    const ok = wsStream.write(frame);
    if (!ok) {
      pauseAllTcpReads();
    }
  };

  const sendStreamError = (streamId: number, code: TcpMuxErrorCode, message: string) => {
    sendMuxFrame(TcpMuxMsgType.ERROR, streamId, encodeTcpMuxErrorPayload(code, message));
  };

  const enqueueStreamWrite = (stream: TcpMuxStreamState, chunk: Buffer) => {
    stream.pendingWrites.push(chunk);
    stream.pendingWriteBytes += chunk.length;
    if (stream.pendingWriteBytes > config.tcpMuxMaxStreamBufferedBytes) {
      sendStreamError(stream.id, TcpMuxErrorCode.STREAM_BUFFER_OVERFLOW, "stream buffered too much data");
      destroyStream(stream.id);
    }
  };

  const flushStreamWrites = (stream: TcpMuxStreamState) => {
    if (closed) return;
    if (!stream.socket) return;
    if (!stream.connected) return;
    if (stream.writePaused) return;

    while (stream.pendingWrites.length > 0) {
      const chunk = stream.pendingWrites.shift()!;
      stream.pendingWriteBytes -= chunk.length;
      const ok = stream.socket.write(chunk);
      if (!ok) {
        stream.writePaused = true;
        break;
      }
    }

    if (stream.clientFin && !stream.clientFinSent && stream.pendingWrites.length === 0) {
      stream.clientFinSent = true;
      stream.socket.end();
    }
  };

  const handleOpen = (frame: TcpMuxFrame) => {
    if (frame.streamId === 0) {
      sendStreamError(frame.streamId, TcpMuxErrorCode.PROTOCOL_ERROR, "stream_id=0 is reserved");
      return;
    }
    if (usedStreamIds.has(frame.streamId)) {
      sendStreamError(frame.streamId, TcpMuxErrorCode.PROTOCOL_ERROR, "stream_id already used");
      return;
    }

    if (streams.size >= config.tcpMuxMaxStreams) {
      sendStreamError(frame.streamId, TcpMuxErrorCode.STREAM_LIMIT_EXCEEDED, "max streams exceeded");
      return;
    }

    let target: { host: string; port: number };
    try {
      target = decodeTcpMuxOpenPayload(frame.payload);
    } catch (err) {
      sendStreamError(frame.streamId, TcpMuxErrorCode.PROTOCOL_ERROR, (err as Error).message);
      return;
    }

    const host = normalizeTargetHostForPolicy(target.host);
    const port = target.port;

    if (host.trim() === "" || !Number.isInteger(port) || port < 1 || port > 65535) {
      sendStreamError(frame.streamId, TcpMuxErrorCode.PROTOCOL_ERROR, "Invalid host or port");
      return;
    }

    usedStreamIds.add(frame.streamId);

    log("info", "connect_requested", {
      connId,
      proto: "tcp-mux",
      streamId: frame.streamId,
      host,
      port,
      clientAddress
    });

    const stream: TcpMuxStreamState = {
      id: frame.streamId,
      host,
      port,
      socket: null,
      connected: false,
      clientFin: false,
      clientFinSent: false,
      serverFin: false,
      pendingWrites: [],
      pendingWriteBytes: 0,
      writePaused: false,
      connectTimer: null
    };

    streams.set(stream.id, stream);

    void (async () => {
      let decision;
      try {
        decision = await resolveAndAuthorizeTarget(host, port, {
          open: config.open,
          allowlist: config.allow,
          dnsTimeoutMs: config.dnsTimeoutMs
        });
      } catch (err) {
        metrics.incConnectionError("error");
        sendStreamError(stream.id, TcpMuxErrorCode.DIAL_FAILED, (err as Error).message);
        destroyStream(stream.id);
        log("error", "connect_error", {
          connId,
          proto: "tcp-mux",
          streamId: stream.id,
          host,
          port,
          clientAddress,
          err: formatError(err)
        });
        return;
      }

      if (closed) return;
      const current = streams.get(stream.id);
      if (!current) return;

      if (!decision.allowed) {
        metrics.incConnectionError("denied");
        sendStreamError(stream.id, TcpMuxErrorCode.POLICY_DENIED, decision.reason);
        destroyStream(stream.id);
        log("warn", "connect_denied", {
          connId,
          proto: "tcp-mux",
          streamId: stream.id,
          host,
          port,
          clientAddress,
          reason: decision.reason
        });
        return;
      }

      log("info", "connect_accepted", {
        connId,
        proto: "tcp-mux",
        streamId: stream.id,
        host,
        port,
        clientAddress,
        resolvedAddress: decision.target.resolvedAddress,
        family: decision.target.family,
        decision: decision.target.decision
      });

      const tcpSocket = net.createConnection({
        host: decision.target.resolvedAddress,
        family: decision.target.family,
        port,
        allowHalfOpen: true
      });
      tcpSocket.setNoDelay(true);
      metrics.tcpMuxStreamsActiveInc();

      current.socket = tcpSocket;
      if (pausedForWsBackpressure) {
        tcpSocket.pause();
      }

      const connectTimer = setTimeout(() => {
        tcpSocket.destroy(new Error(`Connect timeout after ${config.connectTimeoutMs}ms`));
      }, config.connectTimeoutMs);
      connectTimer.unref();
      current.connectTimer = connectTimer;

      tcpSocket.once("connect", () => {
        clearTimeout(connectTimer);
        current.connectTimer = null;
        current.connected = true;
        flushStreamWrites(current);
      });

      tcpSocket.on("data", (chunk) => {
        bytesOut += chunk.length;
        metrics.addBytesOut("tcp_mux", chunk.length);
        sendMuxFrame(TcpMuxMsgType.DATA, current.id, chunk);
      });

      tcpSocket.on("drain", () => {
        current.writePaused = false;
        flushStreamWrites(current);
      });

      tcpSocket.on("end", () => {
        if (current.serverFin) return;
        current.serverFin = true;
        sendMuxFrame(TcpMuxMsgType.CLOSE, current.id, encodeTcpMuxClosePayload(TcpMuxCloseFlags.FIN));
      });

      tcpSocket.on("error", (err) => {
        metrics.incConnectionError("error");
        sendStreamError(current.id, TcpMuxErrorCode.DIAL_FAILED, (err as Error).message);
        destroyStream(current.id);
        log("error", "connect_error", {
          connId,
          proto: "tcp-mux",
          streamId: current.id,
          host,
          port,
          clientAddress,
          err: formatError(err)
        });
      });

      tcpSocket.on("close", () => {
        metrics.tcpMuxStreamsActiveDec();
        streams.delete(current.id);
        if (current.connectTimer) {
          clearTimeout(current.connectTimer);
          current.connectTimer = null;
        }
      });
    })();
  };

  const handleData = (frame: TcpMuxFrame) => {
    const stream = streams.get(frame.streamId);
    if (!stream) {
      sendStreamError(frame.streamId, TcpMuxErrorCode.UNKNOWN_STREAM, "unknown stream");
      return;
    }
    if (stream.clientFin) {
      sendStreamError(frame.streamId, TcpMuxErrorCode.PROTOCOL_ERROR, "stream is half-closed (client FIN)");
      return;
    }

    bytesIn += frame.payload.length;
    metrics.addBytesIn("tcp_mux", frame.payload.length);

    if (!stream.socket || !stream.connected || stream.writePaused) {
      enqueueStreamWrite(stream, frame.payload);
      return;
    }

    const ok = stream.socket.write(frame.payload);
    if (!ok) {
      stream.writePaused = true;
    }
  };

  const handleClose = (frame: TcpMuxFrame) => {
    const stream = streams.get(frame.streamId);
    if (!stream) return;

    let flags: number;
    try {
      flags = decodeTcpMuxClosePayload(frame.payload).flags;
    } catch (err) {
      sendStreamError(frame.streamId, TcpMuxErrorCode.PROTOCOL_ERROR, (err as Error).message);
      return;
    }

    if ((flags & TcpMuxCloseFlags.RST) !== 0) {
      destroyStream(frame.streamId);
      return;
    }

    if ((flags & TcpMuxCloseFlags.FIN) !== 0) {
      stream.clientFin = true;
      flushStreamWrites(stream);
    }
  };

  const handleMuxFrame = (frame: TcpMuxFrame) => {
    switch (frame.msgType) {
      case TcpMuxMsgType.OPEN: {
        handleOpen(frame);
        return;
      }
      case TcpMuxMsgType.DATA: {
        handleData(frame);
        return;
      }
      case TcpMuxMsgType.CLOSE: {
        handleClose(frame);
        return;
      }
      case TcpMuxMsgType.ERROR: {
        // Not used by v1 clients; ignore.
        return;
      }
      case TcpMuxMsgType.PING: {
        sendMuxFrame(TcpMuxMsgType.PONG, frame.streamId, frame.payload);
        return;
      }
      case TcpMuxMsgType.PONG: {
        return;
      }
      default: {
        sendStreamError(frame.streamId, TcpMuxErrorCode.PROTOCOL_ERROR, `Unknown msg_type ${frame.msgType}`);
      }
    }
  };

  // Drain the `createWebSocketStream` readable side so it doesn't pause the underlying WebSocket.
  // We handle incoming messages via `ws.on("message")` so we can reliably detect text vs binary.
  wsStream.on("data", () => {
    // ignore
  });

  ws.on("message", (data, isBinary) => {
    if (closed) return;
    if (!isBinary) {
      closeAll("ws_text", 1003, "WebSocket text messages are not supported");
      return;
    }

    const buf = Buffer.isBuffer(data)
      ? data
      : Array.isArray(data)
        ? Buffer.concat(data)
        : Buffer.from(data as ArrayBuffer);

    let frames: TcpMuxFrame[];
    try {
      frames = muxParser.push(buf);
    } catch {
      closeAll("protocol_error", 1002, "Protocol error");
      return;
    }

    for (const frame of frames) {
      handleMuxFrame(frame);
    }

    // Avoid unbounded buffering if the peer sends an incomplete frame or never finishes a
    // max-sized payload. The only legitimate "pending" state is a single partial frame.
    if (muxParser.pendingBytes() > TCP_MUX_HEADER_BYTES + config.tcpMuxMaxFramePayloadBytes) {
      closeAll("protocol_error", 1002, "Protocol error");
    }
  });

  wsStream.on("drain", () => {
    if (closed) return;
    resumeAllTcpReads();
  });

  wsStream.on("error", (err) => {
    closeAll("ws_stream_error", 1011, "WebSocket stream error");
    metrics.incConnectionError("error");
    log("error", "connect_error", { connId, proto: "tcp-mux", clientAddress, err: formatError(err) });
  });

  ws.once("close", (code, reason) => {
    wsStream.destroy();
    closeAll("ws_close", code, reason.toString());
  });

  ws.once("error", (err) => {
    closeAll("ws_error", 1011, "WebSocket error");
    metrics.incConnectionError("error");
    log("error", "connect_error", { connId, proto: "tcp-mux", clientAddress, err: formatError(err) });
  });
}

