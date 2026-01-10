import http from "node:http";
import net from "node:net";
import WebSocket, { WebSocketServer } from "ws";
import {
  decodeFrame,
  encodeClose,
  encodeData,
  encodeError,
  encodeOpenAck,
  ErrorCode,
  FrameType,
} from "./protocol.js";

function normalizeRemoteAddress(remoteAddress) {
  if (!remoteAddress) return "unknown";
  // "::ffff:127.0.0.1" -> "127.0.0.1"
  if (remoteAddress.startsWith("::ffff:")) return remoteAddress.slice("::ffff:".length);
  return remoteAddress;
}

function ipv4BytesToString(ip) {
  return `${ip[0]}.${ip[1]}.${ip[2]}.${ip[3]}`;
}

function u32FromIpv4Bytes(ip) {
  // eslint-disable-next-line no-bitwise
  return (((ip[0] << 24) | (ip[1] << 16) | (ip[2] << 8) | ip[3]) >>> 0);
}

function parseCidrV4(cidr) {
  const [ipStr, prefixStr] = cidr.split("/");
  const prefix = Number(prefixStr);
  if (!Number.isInteger(prefix) || prefix < 0 || prefix > 32) throw new Error(`Invalid CIDR prefix: ${cidr}`);
  const parts = ipStr.split(".").map((p) => Number(p));
  if (parts.length !== 4 || parts.some((p) => !Number.isInteger(p) || p < 0 || p > 255)) {
    throw new Error(`Invalid CIDR IPv4 address: ${cidr}`);
  }
  const ip = new Uint8Array(parts);
  const mask = prefix === 0 ? 0 : ((0xffffffff << (32 - prefix)) >>> 0);
  const network = u32FromIpv4Bytes(ip) & mask;
  return { cidr, network, mask };
}

function ipv4InCidr(ipBytes, cidrObj) {
  return (u32FromIpv4Bytes(ipBytes) & cidrObj.mask) === cidrObj.network;
}

function isPrivateIpv4(ip) {
  const a = ip[0];
  const b = ip[1];
  const c = ip[2];

  // 0.0.0.0/8 (this host on this network)
  if (a === 0) return true;
  // 10.0.0.0/8
  if (a === 10) return true;
  // 127.0.0.0/8 (loopback)
  if (a === 127) return true;
  // 169.254.0.0/16 (link-local)
  if (a === 169 && b === 254) return true;
  // 172.16.0.0/12
  if (a === 172 && b >= 16 && b <= 31) return true;
  // 192.168.0.0/16
  if (a === 192 && b === 168) return true;
  // 100.64.0.0/10 (CGNAT)
  if (a === 100 && b >= 64 && b <= 127) return true;

  // Multicast/reserved/broadcast
  if (a >= 224) return true;
  if (a === 192 && b === 0 && c === 0) return true; // 192.0.0.0/24 (IETF protocol assignments)

  return false;
}

class TokenBucket {
  constructor({ refillPerSecond, burst }) {
    this.refillPerSecond = refillPerSecond;
    this.burst = burst;
    this.tokens = burst;
    this.lastRefillMs = Date.now();
  }

  tryConsume(amount) {
    const now = Date.now();
    const elapsed = Math.max(0, now - this.lastRefillMs);
    this.lastRefillMs = now;
    this.tokens = Math.min(this.burst, this.tokens + (elapsed / 1000) * this.refillPerSecond);
    if (this.tokens < amount) return false;
    this.tokens -= amount;
    return true;
  }
}

function defaultLogger(obj) {
  // eslint-disable-next-line no-console
  console.log(JSON.stringify({ time: new Date().toISOString(), ...obj }));
}

export async function createProxyServer(userConfig) {
  const config = {
    host: "127.0.0.1",
    port: 8080,
    path: "/tcp-mux",
    authToken: undefined,
    allowPrivateIps: false,
    allowCidrs: [],
    maxOpenConnectionsPerSession: 256,
    maxFramePayloadBytes: 1024 * 1024,
    // Outbound TCP -> WS backpressure thresholds.
    wsBackpressureHighWatermarkBytes: 8 * 1024 * 1024,
    wsBackpressureLowWatermarkBytes: 4 * 1024 * 1024,
    // Rate limits (per WebSocket session).
    maxOpenRequestsPerMinute: 120,
    maxClientBytesPerSecond: 5 * 1024 * 1024,
    logger: defaultLogger,
    metricsIntervalMs: 10_000,
    ...userConfig,
  };

  if (!config.authToken) throw new Error("authToken is required");

  const allowCidrs = (config.allowCidrs ?? []).map(parseCidrV4);

  const stats = {
    wsConnectionsTotal: 0,
    wsConnectionsActive: 0,
    tcpConnectionsTotal: 0,
    tcpConnectionsActive: 0,
    framesFromClient: 0,
    framesToClient: 0,
    bytesFromClient: 0,
    bytesToClient: 0,
    deniedDestinations: 0,
    rateLimited: 0,
    authFailed: 0,
    wsBackpressurePauses: 0,
    wsBackpressureResumes: 0,
    tcpBackpressurePauses: 0,
    tcpBackpressureResumes: 0,
  };

  const log = config.logger ?? defaultLogger;

  const httpServer = http.createServer((req, res) => {
    res.statusCode = 404;
    res.end();
  });

  const wss = new WebSocketServer({
    noServer: true,
    maxPayload: config.maxFramePayloadBytes + 64, // header overhead
  });

  function rejectUpgrade(socket, statusCode, message) {
    socket.write(`HTTP/1.1 ${statusCode} ${message}\r\n\r\n`);
    socket.destroy();
  }

  httpServer.on("upgrade", (req, socket, head) => {
    try {
      const url = new URL(req.url ?? "/", "http://localhost");
      if (url.pathname !== config.path) {
        rejectUpgrade(socket, 404, "Not Found");
        return;
      }

      const tokenFromQuery = url.searchParams.get("token");
      const authHeader = req.headers.authorization;
      const tokenFromHeader = authHeader?.startsWith("Bearer ") ? authHeader.slice("Bearer ".length) : undefined;
      const token = tokenFromQuery ?? tokenFromHeader;

      if (!token) {
        stats.authFailed += 1;
        rejectUpgrade(socket, 401, "Unauthorized");
        return;
      }
      if (token !== config.authToken) {
        stats.authFailed += 1;
        rejectUpgrade(socket, 403, "Forbidden");
        return;
      }

      wss.handleUpgrade(req, socket, head, (ws) => {
        wss.emit("connection", ws, req);
      });
    } catch {
      rejectUpgrade(socket, 400, "Bad Request");
    }
  });

  wss.on("connection", (ws, req) => {
    stats.wsConnectionsTotal += 1;
    stats.wsConnectionsActive += 1;

    const remoteIp = normalizeRemoteAddress(req.socket.remoteAddress);
    const sessionId = `${remoteIp}-${Math.random().toString(16).slice(2, 10)}`;

    log({ level: "info", event: "ws_connected", sessionId, remoteIp });

    const openBucket = new TokenBucket({
      refillPerSecond: config.maxOpenRequestsPerMinute / 60,
      burst: config.maxOpenRequestsPerMinute,
    });

    const bytesBucket = new TokenBucket({
      refillPerSecond: config.maxClientBytesPerSecond,
      burst: config.maxClientBytesPerSecond * 2,
    });

    /** @type {Map<number, { socket: net.Socket, open: boolean, pausedForWsBackpressure: boolean, closeSent: boolean }>} */
    const conns = new Map();

    /** @type {Uint8Array[]} */
    const wsSendQueue = [];
    let wsSendQueueBytes = 0;
    let wsSendFlushScheduled = false;
    let wsBackpressureActive = false;
    let wsPauseRefCount = 0;

    function backlogBytes() {
      // We track the queue we control (wsSendQueueBytes) and also include the
      // ws library's own internal buffer measurement for extra safety.
      return wsSendQueueBytes + (ws.bufferedAmount ?? 0);
    }

    function maybePauseTcpForWsBackpressure() {
      if (ws.readyState !== WebSocket.OPEN) return;
      if (wsBackpressureActive) return;
      if (backlogBytes() <= config.wsBackpressureHighWatermarkBytes) return;

      let didPauseAny = false;
      for (const [, conn] of conns) {
        if (!conn.open) continue;
        if (conn.pausedForWsBackpressure) continue;
        conn.socket.pause();
        conn.pausedForWsBackpressure = true;
        didPauseAny = true;
      }

      if (!didPauseAny) return;
      wsBackpressureActive = true;
      stats.wsBackpressurePauses += 1;
    }

    function maybeResumeTcpForWsBackpressure() {
      if (!wsBackpressureActive) return;
      if (ws.readyState !== WebSocket.OPEN) return;
      if (backlogBytes() > config.wsBackpressureLowWatermarkBytes) return;

      let didResumeAny = false;
      for (const [, conn] of conns) {
        if (!conn.pausedForWsBackpressure) continue;
        conn.socket.resume();
        conn.pausedForWsBackpressure = false;
        didResumeAny = true;
      }

      wsBackpressureActive = false;
      if (didResumeAny) stats.wsBackpressureResumes += 1;
    }

    function flushWsSendQueue() {
      if (ws.readyState !== WebSocket.OPEN) return;
      while (wsSendQueue.length > 0) {
        const frame = wsSendQueue.shift();
        wsSendQueueBytes -= frame.byteLength;
        ws.send(frame, { binary: true }, () => {});
        // Yield if the ws library is starting to buffer; we'll retry on the
        // next immediate.
        if (ws.bufferedAmount > config.wsBackpressureHighWatermarkBytes) break;
      }
      maybeResumeTcpForWsBackpressure();
      if (wsSendQueue.length > 0) {
        const delayMs = ws.bufferedAmount > config.wsBackpressureHighWatermarkBytes ? 10 : 0;
        scheduleWsFlush(delayMs);
      }
    }

    function scheduleWsFlush(delayMs = 0) {
      if (wsSendFlushScheduled) return;
      wsSendFlushScheduled = true;
      const run = () => {
        wsSendFlushScheduled = false;
        flushWsSendQueue();
      };
      if (delayMs > 0) setTimeout(run, delayMs);
      else setImmediate(run);
    }

    function pauseWsIncoming() {
      const s = ws._socket;
      if (!s) return;
      if (wsPauseRefCount === 0) {
        stats.tcpBackpressurePauses += 1;
        s.pause();
      }
      wsPauseRefCount += 1;
    }

    function resumeWsIncoming() {
      const s = ws._socket;
      if (!s) return;
      wsPauseRefCount = Math.max(0, wsPauseRefCount - 1);
      if (wsPauseRefCount === 0) {
        stats.tcpBackpressureResumes += 1;
        s.resume();
      }
    }

    function sendFrame(frame) {
      if (ws.readyState !== WebSocket.OPEN) return;
      stats.framesToClient += 1;
      stats.bytesToClient += frame.byteLength;
      wsSendQueue.push(frame);
      wsSendQueueBytes += frame.byteLength;
      maybePauseTcpForWsBackpressure();
      scheduleWsFlush();
    }

    function closeConnection(connectionId) {
      const conn = conns.get(connectionId);
      if (!conn) return;

      conns.delete(connectionId);
      stats.tcpConnectionsActive = Math.max(0, stats.tcpConnectionsActive - 1);

      try {
        conn.socket.destroy();
      } catch {
        // ignore
      }

      if (!conn.closeSent) {
        conn.closeSent = true;
        sendFrame(encodeClose(connectionId));
      }
    }

    function failConnection(connectionId, code, message) {
      sendFrame(encodeError(connectionId, code, message));
      closeConnection(connectionId);
    }

    ws.on("message", (data) => {
      stats.framesFromClient += 1;
      stats.bytesFromClient += data.byteLength ?? data.length ?? 0;

      let frame;
      try {
        frame = decodeFrame(data);
      } catch (e) {
        sendFrame(encodeError(0, ErrorCode.INVALID_FRAME, String(e?.message ?? e)));
        ws.close();
        return;
      }

      if (frame.type === FrameType.OPEN) {
        if (frame.kind !== "request") {
          sendFrame(encodeError(frame.connectionId, ErrorCode.INVALID_FRAME, "OPEN ack from client"));
          return;
        }

        if (!openBucket.tryConsume(1)) {
          stats.rateLimited += 1;
          failConnection(frame.connectionId, ErrorCode.RATE_LIMITED, "OPEN rate limit");
          return;
        }

        if (conns.size >= config.maxOpenConnectionsPerSession) {
          failConnection(frame.connectionId, ErrorCode.RATE_LIMITED, "Too many open connections");
          return;
        }
        if (conns.has(frame.connectionId)) {
          failConnection(frame.connectionId, ErrorCode.INVALID_FRAME, "Duplicate connection_id");
          return;
        }

        const dstIp = frame.dstIp;
        const dstPort = frame.dstPort;
        const dstIpStr = ipv4BytesToString(dstIp);

        const isPrivate = isPrivateIpv4(dstIp);
        const explicitlyAllowed = allowCidrs.some((cidr) => ipv4InCidr(dstIp, cidr));
        if (isPrivate && !config.allowPrivateIps && !explicitlyAllowed) {
          stats.deniedDestinations += 1;
          log({ level: "warn", event: "policy_denied", sessionId, dstIp: dstIpStr, dstPort });
          failConnection(frame.connectionId, ErrorCode.POLICY_DENIED, "Destination is in a private/reserved IPv4 range");
          return;
        }

        stats.tcpConnectionsTotal += 1;
        stats.tcpConnectionsActive += 1;

        log({ level: "info", event: "tcp_connect_start", sessionId, connectionId: frame.connectionId, dstIp: dstIpStr, dstPort });

        const socket = net.createConnection({ host: dstIpStr, port: dstPort });
        socket.setNoDelay(true);

        const connState = { socket, open: false, pausedForWsBackpressure: false, closeSent: false };
        conns.set(frame.connectionId, connState);

        socket.on("connect", () => {
          connState.open = true;
          log({ level: "info", event: "tcp_connected", sessionId, connectionId: frame.connectionId, dstIp: dstIpStr, dstPort });
          sendFrame(encodeOpenAck(frame.connectionId));
        });

        socket.on("data", (chunk) => {
          sendFrame(encodeData(frame.connectionId, chunk));
        });

        socket.on("error", (err) => {
          log({
            level: "warn",
            event: "tcp_error",
            sessionId,
            connectionId: frame.connectionId,
            message: String(err?.message ?? err),
          });
          failConnection(frame.connectionId, ErrorCode.SOCKET_ERROR, "TCP socket error");
        });

        socket.on("close", () => {
          log({ level: "info", event: "tcp_closed", sessionId, connectionId: frame.connectionId });
          closeConnection(frame.connectionId);
        });

        return;
      }

      if (frame.type === FrameType.DATA) {
        const conn = conns.get(frame.connectionId);
        if (!conn || !conn.open) {
          sendFrame(encodeError(frame.connectionId, ErrorCode.INVALID_FRAME, "DATA for unknown connection"));
          return;
        }

        const byteLen = frame.data.byteLength ?? frame.data.length ?? 0;
        if (!bytesBucket.tryConsume(byteLen)) {
          stats.rateLimited += 1;
          failConnection(frame.connectionId, ErrorCode.RATE_LIMITED, "DATA rate limit");
          return;
        }

        const ok = conn.socket.write(frame.data);
        if (!ok) {
          pauseWsIncoming();
          conn.socket.once("drain", () => {
            resumeWsIncoming();
          });
        }
        return;
      }

      if (frame.type === FrameType.CLOSE) {
        closeConnection(frame.connectionId);
        return;
      }

      // Client should never send ERROR frames.
      sendFrame(encodeError(frame.connectionId, ErrorCode.INVALID_FRAME, "Unexpected frame type from client"));
    });

    ws.on("close", () => {
      stats.wsConnectionsActive = Math.max(0, stats.wsConnectionsActive - 1);
      log({ level: "info", event: "ws_closed", sessionId });
      for (const [id] of conns) closeConnection(id);
    });

    ws.on("error", (err) => {
      log({ level: "warn", event: "ws_error", sessionId, message: String(err?.message ?? err) });
    });
  });

  let metricsTimer = null;
  if (config.metricsIntervalMs > 0) {
    metricsTimer = setInterval(() => {
      log({ level: "info", event: "metrics", ...stats });
    }, config.metricsIntervalMs);
  }

  await new Promise((resolve) => httpServer.listen(config.port, config.host, resolve));
  const address = httpServer.address();
  const port = typeof address === "object" ? address.port : config.port;

  return {
    url: `ws://${config.host}:${port}${config.path}`,
    stats,
    async close() {
      if (metricsTimer) clearInterval(metricsTimer);
      await new Promise((resolve) => httpServer.close(resolve));
      wss.close();
    },
  };
}
