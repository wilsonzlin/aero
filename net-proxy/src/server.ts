import http from "node:http";
import net from "node:net";
import dgram from "node:dgram";
import { PassThrough } from "node:stream";
import { createWebSocketStream, WebSocketServer, type WebSocket } from "ws";
import { loadConfigFromEnv, type ProxyConfig } from "./config";
import { formatError, log } from "./logger";
import { resolveAndAuthorizeTarget } from "./security";

export interface RunningProxyServer {
  server: http.Server;
  config: ProxyConfig;
  listenAddress: string;
  close: () => Promise<void>;
}

export async function startProxyServer(overrides: Partial<ProxyConfig> = {}): Promise<RunningProxyServer> {
  const config: ProxyConfig = { ...loadConfigFromEnv(), ...overrides };

  const server = http.createServer((req, res) => {
    const url = new URL(req.url ?? "/", "http://localhost");
    if (req.method === "GET" && url.pathname === "/healthz") {
      const body = JSON.stringify({ ok: true });
      res.writeHead(200, {
        "content-type": "application/json; charset=utf-8",
        "content-length": Buffer.byteLength(body)
      });
      res.end(body);
      return;
    }

    res.writeHead(404, { "content-type": "application/json; charset=utf-8" });
    res.end(JSON.stringify({ error: "not found" }));
  });

  const wss = new WebSocketServer({ noServer: true, maxPayload: config.wsMaxPayloadBytes });
  let nextConnId = 1;

  server.on("upgrade", (req, socket, head) => {
    const url = new URL(req.url ?? "/", "http://localhost");
    if (url.pathname !== "/tcp" && url.pathname !== "/udp") {
      socket.destroy();
      return;
    }

    wss.handleUpgrade(req, socket, head, (ws) => {
      wss.emit("connection", ws, req);
    });
  });

  wss.on("connection", (ws, req) => {
    const connId = nextConnId++;
    const url = new URL(req.url ?? "/", "http://localhost");
    const proto = url.pathname === "/udp" ? "udp" : "tcp";

    const host = url.searchParams.get("host") ?? "";
    const portRaw = url.searchParams.get("port") ?? "";
    const port = Number(portRaw);
    const clientAddress = req.socket.remoteAddress ?? null;

    log("info", "connect_requested", { connId, proto, host, port: portRaw, clientAddress });

    if (host.trim() === "" || !Number.isInteger(port) || port < 1 || port > 65535) {
      log("warn", "connect_denied", {
        connId,
        proto,
        host,
        port: portRaw,
        clientAddress,
        reason: "Invalid host or port"
      });
      ws.close(1008, "Invalid host or port");
      return;
    }

    void (async () => {
      try {
        const decision = await resolveAndAuthorizeTarget(host, port, {
          open: config.open,
          allowlist: config.allow,
          dnsTimeoutMs: config.dnsTimeoutMs
        });

        if (!decision.allowed) {
          log("warn", "connect_denied", { connId, proto, host, port, clientAddress, reason: decision.reason });
          ws.close(1008, decision.reason);
          return;
        }

        log("info", "connect_accepted", {
          connId,
          proto,
          host,
          port,
          clientAddress,
          resolvedAddress: decision.target.resolvedAddress,
          family: decision.target.family,
          decision: decision.target.decision
        });

        if (proto === "udp") {
          await handleUdpRelay(ws, connId, decision.target.resolvedAddress, decision.target.family, port, config);
        } else {
          await handleTcpRelay(ws, connId, decision.target.resolvedAddress, decision.target.family, port, config);
        }
      } catch (err) {
        log("error", "connect_error", { connId, proto, host, port, clientAddress, err: formatError(err) });
        ws.close(1011, "Proxy error");
      }
    })();
  });

  await new Promise<void>((resolve, reject) => {
    server.once("error", reject);
    server.listen(config.listenPort, config.listenHost, () => resolve());
  });

  const addr = server.address();
  const listenAddress =
    typeof addr === "string" ? addr : `http://${addr?.address ?? config.listenHost}:${addr?.port ?? config.listenPort}`;

  log("info", "proxy_start", { listenAddress, open: config.open, allow: config.allow });

  return {
    server,
    config,
    listenAddress,
    close: async () => {
      log("info", "proxy_stop");
      await new Promise<void>((resolve) => wss.close(() => resolve()));
      await new Promise<void>((resolve, reject) => server.close((err) => (err ? reject(err) : resolve())));
    }
  };
}

async function handleTcpRelay(
  ws: WebSocket,
  connId: number,
  address: string,
  family: 4 | 6,
  port: number,
  config: ProxyConfig
): Promise<void> {
  const wsStream = createWebSocketStream(ws, { highWaterMark: config.wsStreamHighWaterMarkBytes });

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

    clearTimeout(connectTimer);

    if (ws.readyState === ws.OPEN) {
      ws.close(wsCode, wsReason);
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
    log("error", "connect_error", { connId, proto: "tcp", err: formatError(err) });
  });

  tcpSocket.once("connect", () => {
    clearTimeout(connectTimer);
  });

  tcpSocket.once("error", (err) => {
    closeBoth("tcp_error", 1011, "TCP error");
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
  });
  fromTcp.on("data", (chunk) => {
    bytesOut += chunk.length;
  });

  wsStream.pipe(fromWs).pipe(tcpSocket);
  tcpSocket.pipe(fromTcp).pipe(wsStream);
}

async function handleUdpRelay(
  ws: WebSocket,
  connId: number,
  address: string,
  family: 4 | 6,
  port: number,
  config: ProxyConfig
): Promise<void> {
  const socket = dgram.createSocket(family === 6 ? "udp6" : "udp4");
  socket.connect(port, address);

  let bytesIn = 0;
  let bytesOut = 0;
  let closed = false;

  const closeBoth = (why: string, wsCode: number, wsReason: string) => {
    if (closed) return;
    closed = true;
    try {
      socket.close();
    } catch {
      // ignore
    }

    if (ws.readyState === ws.OPEN) {
      ws.close(wsCode, wsReason);
    }

    log("info", "conn_close", {
      connId,
      proto: "udp",
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
    try {
      socket.close();
    } catch {
      // ignore
    }

    log("info", "conn_close", {
      connId,
      proto: "udp",
      why: "ws_close",
      bytesIn,
      bytesOut,
      wsCode: code,
      wsReason: reason.toString()
    });
  });

  ws.once("error", (err) => {
    closeBoth("ws_error", 1011, "WebSocket error");
    log("error", "connect_error", { connId, proto: "udp", err: formatError(err) });
  });

  socket.on("error", (err) => {
    closeBoth("udp_error", 1011, "UDP error");
    log("error", "connect_error", { connId, proto: "udp", err: formatError(err) });
  });

  socket.on("message", (msg) => {
    bytesOut += msg.length;
    if (ws.readyState !== ws.OPEN) return;

    if (ws.bufferedAmount > config.udpWsBufferedAmountLimitBytes) {
      log("warn", "udp_drop_backpressure", {
        connId,
        bufferedAmount: ws.bufferedAmount,
        limit: config.udpWsBufferedAmountLimitBytes,
        droppedBytes: msg.length
      });
      return;
    }

    ws.send(msg);
  });

  ws.on("message", (data, isBinary) => {
    if (!isBinary) return;
    const buf = Buffer.isBuffer(data) ? data : Buffer.from(data as ArrayBuffer);
    bytesIn += buf.length;
    socket.send(buf);
  });
}

