import net from "node:net";
import { PassThrough } from "node:stream";
import { createWebSocketStream, type WebSocket } from "ws";

import type { ProxyConfig } from "./config";
import type { ProxyServerMetrics } from "./metrics";
import { formatError, log } from "./logger";
import { wsCloseSafe } from "./wsClose";

export async function handleTcpRelay(
  ws: WebSocket,
  connId: number,
  address: string,
  family: 4 | 6,
  port: number,
  config: ProxyConfig,
  metrics: ProxyServerMetrics
): Promise<void> {
  if (ws.readyState !== ws.OPEN) return;

  const wsStream = createWebSocketStream(ws, { highWaterMark: config.wsStreamHighWaterMarkBytes });

  metrics.connectionActiveInc("tcp");

  let bytesIn = 0;
  let bytesOut = 0;
  let closed = false;

  const tcpSocket = net.createConnection({ host: address, port, family });
  tcpSocket.setNoDelay(true);

  const connectTimer = setTimeout(() => {
    tcpSocket.destroy(new Error(`Connect timeout after ${config.connectTimeoutMs}ms`));
  }, config.connectTimeoutMs);
  connectTimer.unref();

  const closeBoth = (why: string, wsCode: number, wsReason: string) => {
    if (closed) return;
    closed = true;
    metrics.connectionActiveDec("tcp");

    clearTimeout(connectTimer);

    if (ws.readyState === ws.OPEN) {
      wsCloseSafe(ws, wsCode, wsReason);
    }

    tcpSocket.destroy();
    wsStream.destroy();

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

  ws.once("close", (code, reason) => {
    if (closed) return;
    closed = true;
    metrics.connectionActiveDec("tcp");
    clearTimeout(connectTimer);
    tcpSocket.destroy();
    wsStream.destroy();

    log("info", "conn_close", {
      connId,
      proto: "tcp",
      why: "ws_close",
      bytesIn,
      bytesOut,
      wsCode: code,
      wsReason: reason.toString()
    });
  });

  ws.once("error", (err) => {
    closeBoth("ws_error", 1011, "WebSocket error");
    metrics.incConnectionError("error");
    log("error", "connect_error", { connId, proto: "tcp", err: formatError(err) });
  });

  tcpSocket.once("connect", () => {
    clearTimeout(connectTimer);
  });

  tcpSocket.once("error", (err) => {
    closeBoth("tcp_error", 1011, "TCP error");
    metrics.incConnectionError("error");
    log("error", "connect_error", { connId, proto: "tcp", err: formatError(err) });
  });

  tcpSocket.once("close", () => {
    if (!closed) {
      closeBoth("tcp_close", 1000, "TCP closed");
    }
  });

  const fromWs = new PassThrough();
  const fromTcp = new PassThrough();

  fromWs.on("data", (chunk) => {
    bytesIn += chunk.length;
    metrics.addBytesIn("tcp", chunk.length);
  });
  fromTcp.on("data", (chunk) => {
    bytesOut += chunk.length;
    metrics.addBytesOut("tcp", chunk.length);
  });

  wsStream.pipe(fromWs).pipe(tcpSocket);
  tcpSocket.pipe(fromTcp).pipe(wsStream);
}

