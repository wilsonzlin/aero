import net from "node:net";
import { createWebSocketStream, type WebSocket } from "ws";

import { socketWritableLengthExceedsCap, socketWritableLengthOrOverflow } from "./socketWritableLength";
import { unrefBestEffort } from "./unrefSafe";

import type { ProxyConfig } from "./config";
import type { ProxyServerMetrics } from "./metrics";
import { formatError, log } from "./logger";
import {
  destroyBestEffort,
  destroyWithErrorBestEffort,
  pauseBestEffort,
  resumeBestEffort,
  writeCaptureErrorBestEffort,
} from "./socketSafe";
import { wsCloseSafe, wsIsOpenSafe } from "./wsClose";
import { formatOneLineUtf8 } from "./text";

export async function handleTcpRelay(
  ws: WebSocket,
  connId: number,
  address: string,
  family: 4 | 6,
  port: number,
  config: ProxyConfig,
  metrics: ProxyServerMetrics
): Promise<void> {
  if (!wsIsOpenSafe(ws)) return;

  let bytesIn = 0;
  let bytesOut = 0;
  let closed = false;
  let active = false;
  let pausedWsReadForTcpBackpressure = false;
  let pausedTcpReadForWsBackpressure = false;

  const createConnection = config.createTcpConnection ?? net.createConnection;
  let wsStream: ReturnType<typeof createWebSocketStream>;
  try {
    wsStream = createWebSocketStream(ws, { highWaterMark: config.wsStreamHighWaterMarkBytes });
  } catch (err) {
    metrics.incConnectionError("error");
    wsCloseSafe(ws, 1011, "WebSocket stream error");
    log("error", "connect_error", { connId, proto: "tcp", err: formatError(err) });
    return;
  }

  let tcpSocket: net.Socket | null = null;
  let connectTimer: NodeJS.Timeout | null = null;

  const closeBoth = (why: string, wsCode: number, wsReason: string) => {
    if (closed) return;
    closed = true;
    if (active) {
      active = false;
      metrics.connectionActiveDec("tcp");
    }

    if (connectTimer) {
      clearTimeout(connectTimer);
      connectTimer = null;
    }

    const wsWasOpen = wsIsOpenSafe(ws);
    if (wsWasOpen) {
      wsCloseSafe(ws, wsCode, wsReason);
    }

    if (tcpSocket) {
      destroyBestEffort(tcpSocket);
    }
    // Avoid destroying the wrapper stream before we've had a chance to send a close frame.
    // If the websocket isn't open, tear it down immediately to avoid leaks.
    if (!wsWasOpen) {
      destroyBestEffort(wsStream);
    }

    log("info", "conn_close", {
      connId,
      proto: "tcp",
      why,
      bytesIn,
      bytesOut,
      wsCode,
      wsReason
    });
  };

  try {
    tcpSocket = createConnection({ host: address, port, family });
    tcpSocket.setNoDelay(true);
  } catch (err) {
    closeBoth("tcp_create_error", 1011, "TCP error");
    metrics.incConnectionError("error");
    log("error", "connect_error", { connId, proto: "tcp", err: formatError(err) });
    return;
  }

  const socket = tcpSocket;
  active = true;
  metrics.connectionActiveInc("tcp");

  connectTimer = setTimeout(() => {
    destroyWithErrorBestEffort(socket, new Error(`Connect timeout after ${config.connectTimeoutMs}ms`));
  }, config.connectTimeoutMs);
  unrefBestEffort(connectTimer);

  ws.once("close", (code, reason) => {
    closeBoth("ws_close", code, formatOneLineUtf8(reason, 123));
  });

  ws.once("error", (err) => {
    closeBoth("ws_error", 1011, "WebSocket error");
    metrics.incConnectionError("error");
    log("error", "connect_error", { connId, proto: "tcp", err: formatError(err) });
  });

  socket.once("connect", () => {
    if (connectTimer) {
      clearTimeout(connectTimer);
      connectTimer = null;
    }
  });

  socket.once("error", (err) => {
    closeBoth("tcp_error", 1011, "TCP error");
    metrics.incConnectionError("error");
    log("error", "connect_error", { connId, proto: "tcp", err: formatError(err) });
  });

  socket.once("close", () => {
    if (!closed) {
      closeBoth("tcp_close", 1000, "TCP closed");
    }
  });

  wsStream.once("error", (err) => {
    closeBoth("ws_stream_error", 1011, "WebSocket stream error");
    metrics.incConnectionError("error");
    log("error", "connect_error", { connId, proto: "tcp", err: formatError(err) });
  });

  const pauseWsRead = () => {
    if (pausedWsReadForTcpBackpressure) return;
    pausedWsReadForTcpBackpressure = true;
    pauseBestEffort(wsStream);
  };

  const resumeWsRead = () => {
    if (!pausedWsReadForTcpBackpressure) return;
    pausedWsReadForTcpBackpressure = false;
    resumeBestEffort(wsStream);
  };

  const pauseTcpRead = () => {
    if (pausedTcpReadForWsBackpressure) return;
    pausedTcpReadForWsBackpressure = true;
    pauseBestEffort(socket);
  };

  const resumeTcpRead = () => {
    if (!pausedTcpReadForWsBackpressure) return;
    pausedTcpReadForWsBackpressure = false;
    resumeBestEffort(socket);
  };

  const enforceTcpBackpressureCap = () => {
    const cap = config.maxTcpBufferedBytesPerConn;
    if (!socketWritableLengthExceedsCap(socket, cap)) return;
    const buffered = socketWritableLengthOrOverflow(socket, cap);
    closeBoth("tcp_buffer_overflow", 1011, "TCP buffered too much data");
    metrics.incConnectionError("error");
    log("warn", "connect_error", {
      connId,
      proto: "tcp",
      err: formatError(new Error(`tcp writableLength exceeded cap (${buffered} > ${cap})`))
    });
  };

  socket.on("drain", () => {
    if (closed) return;
    resumeWsRead();
  });

  wsStream.on("drain", () => {
    if (closed) return;
    resumeTcpRead();
  });

  wsStream.on("data", (chunk) => {
    if (closed) return;
    const buf = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk as any);
    bytesIn += buf.length;
    metrics.addBytesIn("tcp", buf.length);

    const res = writeCaptureErrorBestEffort(socket, buf);
    if (res.err) {
      closeBoth("tcp_write_error", 1011, "TCP error");
      metrics.incConnectionError("error");
      log("error", "connect_error", { connId, proto: "tcp", err: formatError(res.err) });
      return;
    }
    enforceTcpBackpressureCap();
    if (!res.ok) pauseWsRead();
  });

  socket.on("data", (chunk) => {
    if (closed) return;
    bytesOut += chunk.length;
    metrics.addBytesOut("tcp", chunk.length);

    const res = writeCaptureErrorBestEffort(wsStream, chunk);
    if (res.err) {
      closeBoth("ws_stream_write_error", 1011, "WebSocket stream error");
      metrics.incConnectionError("error");
      log("error", "connect_error", { connId, proto: "tcp", err: formatError(res.err) });
      return;
    }
    if (!res.ok) pauseTcpRead();
  });
}

