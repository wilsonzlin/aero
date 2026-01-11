import http from "node:http";
import net from "node:net";
import dgram from "node:dgram";
import { PassThrough } from "node:stream";
import { createWebSocketStream, WebSocketServer, type WebSocket } from "ws";
import { loadConfigFromEnv, type ProxyConfig } from "./config";
import { formatError, log } from "./logger";
import { resolveAndAuthorizeTarget } from "./security";
import {
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

export interface RunningProxyServer {
  server: http.Server;
  config: ProxyConfig;
  listenAddress: string;
  close: () => Promise<void>;
}

function truncateCloseReason(reason: string, maxBytes = 123): string {
  const buf = Buffer.from(reason, "utf8");
  if (buf.length <= maxBytes) return reason;

  let truncated = buf.subarray(0, maxBytes).toString("utf8");
  while (Buffer.byteLength(truncated, "utf8") > maxBytes) {
    truncated = truncated.slice(0, -1);
  }
  return truncated;
}

function wsCloseSafe(ws: WebSocket, code: number, reason: string): void {
  const safeReason = truncateCloseReason(reason);
  ws.close(code, safeReason);
}

function stripOptionalIpv6Brackets(host: string): string {
  const trimmed = host.trim();
  if (trimmed.startsWith("[") && trimmed.endsWith("]")) {
    return trimmed.slice(1, -1);
  }
  return trimmed;
}

function parseTargetQuery(url: URL): { host: string; port: number; portRaw: string } | { error: string } {
  const hostRaw = url.searchParams.get("host");
  const portRaw = url.searchParams.get("port");
  if (hostRaw !== null && portRaw !== null) {
    const port = Number(portRaw);
    if (hostRaw.trim() === "" || !Number.isInteger(port) || port < 1 || port > 65535) {
      return { error: "Invalid host or port" };
    }
    return { host: stripOptionalIpv6Brackets(hostRaw), port, portRaw };
  }

  const target = url.searchParams.get("target");
  if (target === null || target.trim() === "") {
    return { error: "Missing host/port (or target)" };
  }

  const t = target.trim();
  let host = "";
  let portPart = "";
  if (t.startsWith("[")) {
    const closeIdx = t.indexOf("]");
    if (closeIdx === -1) return { error: "Invalid target (missing closing ] for IPv6)" };
    host = t.slice(1, closeIdx);
    const rest = t.slice(closeIdx + 1);
    if (!rest.startsWith(":")) return { error: "Invalid target (missing :port)" };
    portPart = rest.slice(1);
  } else {
    const colonIdx = t.lastIndexOf(":");
    if (colonIdx === -1) return { error: "Invalid target (missing :port)" };
    host = t.slice(0, colonIdx);
    portPart = t.slice(colonIdx + 1);
  }

  const port = Number(portPart);
  if (host.trim() === "" || !Number.isInteger(port) || port < 1 || port > 65535) {
    return { error: "Invalid target host or port" };
  }

  return { host: stripOptionalIpv6Brackets(host), port, portRaw: portPart };
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
  const wssMux = new WebSocketServer({
    noServer: true,
    maxPayload: config.wsMaxPayloadBytes,
    handleProtocols: (protocols) => (protocols.has(TCP_MUX_SUBPROTOCOL) ? TCP_MUX_SUBPROTOCOL : false)
  });
  let nextConnId = 1;

  server.on("upgrade", (req, socket, head) => {
    const url = new URL(req.url ?? "/", "http://localhost");
    if (url.pathname === "/tcp-mux") {
      wssMux.handleUpgrade(req, socket, head, (ws) => {
        wssMux.emit("connection", ws, req);
      });
      return;
    }

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

    const parsedTarget = parseTargetQuery(url);
    const clientAddress = req.socket.remoteAddress ?? null;

    if ("error" in parsedTarget) {
      log("warn", "connect_denied", {
        connId,
        proto,
        clientAddress,
        reason: parsedTarget.error
      });
      wsCloseSafe(ws, 1008, parsedTarget.error);
      return;
    }

    const { host, port, portRaw } = parsedTarget;

    log("info", "connect_requested", { connId, proto, host, port: portRaw, clientAddress });

    void (async () => {
      try {
        const decision = await resolveAndAuthorizeTarget(host, port, {
          open: config.open,
          allowlist: config.allow,
          dnsTimeoutMs: config.dnsTimeoutMs
        });

        if (!decision.allowed) {
          log("warn", "connect_denied", { connId, proto, host, port, clientAddress, reason: decision.reason });
          wsCloseSafe(ws, 1008, decision.reason);
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
        wsCloseSafe(ws, 1011, "Proxy error");
      }
    })();
  });

  wssMux.on("connection", (ws, req) => {
    const connId = nextConnId++;
    const clientAddress = req.socket.remoteAddress ?? null;
    handleTcpMuxRelay(ws, connId, clientAddress, config);
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
      await Promise.all([
        new Promise<void>((resolve) => wss.close(() => resolve())),
        new Promise<void>((resolve) => wssMux.close(() => resolve()))
      ]);
      await new Promise<void>((resolve, reject) => server.close((err) => (err ? reject(err) : resolve())));
    }
  };
}

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

function handleTcpMuxRelay(ws: WebSocket, connId: number, clientAddress: string | null, config: ProxyConfig): void {
  if (ws.protocol !== TCP_MUX_SUBPROTOCOL) {
    wsCloseSafe(ws, 1002, `Missing required subprotocol: ${TCP_MUX_SUBPROTOCOL}`);
    return;
  }

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

  const closeAll = (why: string, wsCode: number, wsReason: string) => {
    if (closed) return;
    closed = true;

    for (const streamId of [...streams.keys()]) {
      destroyStream(streamId);
    }

    wsStream.destroy();

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

  const destroyStream = (streamId: number) => {
    const stream = streams.get(streamId);
    if (!stream) return;
    streams.delete(streamId);
    if (stream.connectTimer) {
      clearTimeout(stream.connectTimer);
    }
    if (stream.socket) {
      stream.socket.removeAllListeners();
      stream.socket.destroy();
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

    if (target.host.trim() === "" || !Number.isInteger(target.port) || target.port < 1 || target.port > 65535) {
      sendStreamError(frame.streamId, TcpMuxErrorCode.PROTOCOL_ERROR, "Invalid host or port");
      return;
    }

    usedStreamIds.add(frame.streamId);

    log("info", "connect_requested", {
      connId,
      proto: "tcp-mux",
      streamId: frame.streamId,
      host: target.host,
      port: target.port,
      clientAddress
    });

    const stream: TcpMuxStreamState = {
      id: frame.streamId,
      host: target.host,
      port: target.port,
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
        decision = await resolveAndAuthorizeTarget(target.host, target.port, {
          open: config.open,
          allowlist: config.allow,
          dnsTimeoutMs: config.dnsTimeoutMs
        });
      } catch (err) {
        sendStreamError(stream.id, TcpMuxErrorCode.DIAL_FAILED, (err as Error).message);
        destroyStream(stream.id);
        log("error", "connect_error", {
          connId,
          proto: "tcp-mux",
          streamId: stream.id,
          host: target.host,
          port: target.port,
          clientAddress,
          err: formatError(err)
        });
        return;
      }

      if (closed) return;
      const current = streams.get(stream.id);
      if (!current) return;

      if (!decision.allowed) {
        sendStreamError(stream.id, TcpMuxErrorCode.POLICY_DENIED, decision.reason);
        destroyStream(stream.id);
        log("warn", "connect_denied", {
          connId,
          proto: "tcp-mux",
          streamId: stream.id,
          host: target.host,
          port: target.port,
          clientAddress,
          reason: decision.reason
        });
        return;
      }

      log("info", "connect_accepted", {
        connId,
        proto: "tcp-mux",
        streamId: stream.id,
        host: target.host,
        port: target.port,
        clientAddress,
        resolvedAddress: decision.target.resolvedAddress,
        family: decision.target.family,
        decision: decision.target.decision
      });

      const tcpSocket = net.createConnection({
        host: decision.target.resolvedAddress,
        family: decision.target.family,
        port: target.port,
        allowHalfOpen: true
      });
      tcpSocket.setNoDelay(true);

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
        sendStreamError(current.id, TcpMuxErrorCode.DIAL_FAILED, (err as Error).message);
        destroyStream(current.id);
        log("error", "connect_error", {
          connId,
          proto: "tcp-mux",
          streamId: current.id,
          host: target.host,
          port: target.port,
          clientAddress,
          err: formatError(err)
        });
      });

      tcpSocket.on("close", () => {
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

  wsStream.on("data", (chunk) => {
    if (closed) return;
    if (typeof chunk === "string") {
      closeAll("ws_text", 1003, "WebSocket text messages are not supported");
      return;
    }

    const buf = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk as ArrayBuffer);

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
  });

  wsStream.on("drain", () => {
    if (closed) return;
    resumeAllTcpReads();
  });

  wsStream.on("error", (err) => {
    closeAll("ws_stream_error", 1011, "WebSocket stream error");
    log("error", "connect_error", { connId, proto: "tcp-mux", clientAddress, err: formatError(err) });
  });

  ws.once("close", (code, reason) => {
    closeAll("ws_close", code, reason.toString());
  });

  ws.once("error", (err) => {
    closeAll("ws_error", 1011, "WebSocket error");
    log("error", "connect_error", { connId, proto: "tcp-mux", clientAddress, err: formatError(err) });
  });
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
      wsCloseSafe(ws, wsCode, wsReason);
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
