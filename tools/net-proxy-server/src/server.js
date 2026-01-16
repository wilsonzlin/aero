import http from "node:http";
import { lookup } from "node:dns/promises";
import net from "node:net";
import WebSocket, { WebSocketServer } from "ws";
import { formatOneLineError, formatOneLineUtf8 } from "./text.js";
import { hasWebSocketSubprotocol } from "./wsSubprotocol.js";
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
} from "./protocol.js";

const MAX_REQUEST_URL_LEN = 8 * 1024;
const MAX_AUTH_HEADER_LEN = 4 * 1024;
const MAX_TOKEN_LEN = 4 * 1024;

// RFC6455 close reason is limited to 123 bytes. Sanitize to avoid log/client injection.
const MAX_WS_CLOSE_REASON_BYTES = 123;
// tcp-mux error payload strings can be attacker-influenced; keep messages small and single-line.
// Match the protocol cap (`encodeTcpMuxErrorPayload`).
const MAX_TCP_MUX_ERROR_MESSAGE_BYTES = 1024;
// HTTP status line text should never be attacker-controlled; keep it tiny and single-line.
const MAX_UPGRADE_STATUS_MESSAGE_BYTES = 64;

function wsCloseSafe(ws, code, reason) {
  try {
    ws.close(code, formatOneLineUtf8(reason, MAX_WS_CLOSE_REASON_BYTES));
  } catch {
    // ignore best-effort close failures
  }
}

function formatTcpMuxErrorMessage(err) {
  return formatOneLineError(err, MAX_TCP_MUX_ERROR_MESSAGE_BYTES);
}

function normalizeRemoteAddress(remoteAddress) {
  if (!remoteAddress) return "unknown";
  // "::ffff:127.0.0.1" -> "127.0.0.1"
  if (remoteAddress.startsWith("::ffff:")) return remoteAddress.slice("::ffff:".length);
  return remoteAddress;
}

function singleHeaderValue(value) {
  if (typeof value === "string") return value;
  if (Array.isArray(value)) {
    if (value.length !== 1 || typeof value[0] !== "string") return null;
    return value[0];
  }
  return undefined;
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

function ipv4StringToBytes(ip) {
  const parts = ip.split(".").map((p) => Number(p));
  if (parts.length !== 4 || parts.some((p) => !Number.isInteger(p) || p < 0 || p > 255)) {
    throw new Error(`Invalid IPv4 address: ${ip}`);
  }
  return new Uint8Array(parts);
}

function isPrivateIpv4(ipBytes) {
  const a = ipBytes[0];
  const b = ipBytes[1];
  const c = ipBytes[2];

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

  // 192.0.2.0/24, 198.51.100.0/24, 203.0.113.0/24 (TEST-NET)
  if (a === 192 && b === 0 && c === 2) return true;
  if (a === 198 && b === 51 && c === 100) return true;
  if (a === 203 && b === 0 && c === 113) return true;

  // 198.18.0.0/15 (benchmarking)
  if (a === 198 && (b === 18 || b === 19)) return true;

  // Multicast/reserved/broadcast
  if (a >= 224) return true;
  if (a === 192 && b === 0 && c === 0) return true; // 192.0.0.0/24 (IETF protocol assignments)

  return false;
}

function parseIpv6ToBytes(address) {
  let ip = address;
  const zoneIdx = ip.indexOf("%");
  if (zoneIdx !== -1) ip = ip.slice(0, zoneIdx);
  if (ip.startsWith("[") && ip.endsWith("]")) ip = ip.slice(1, -1);

  const pieces = ip.split("::");
  if (pieces.length > 2) {
    throw new Error(`Invalid IPv6 address: ${address}`);
  }

  const left = pieces[0];
  const right = pieces.length === 2 ? pieces[1] : null;

  const leftParts = left.length > 0 ? left.split(":") : [];
  const rightParts = right && right.length > 0 ? right.split(":") : [];

  const parseParts = (parts) => {
    /** @type {number[]} */
    const out = [];
    for (const part of parts) {
      if (part === "") continue;
      if (part.includes(".")) {
        const v4 = ipv4StringToBytes(part);
        // eslint-disable-next-line no-bitwise
        out.push(((v4[0] << 8) | v4[1]) >>> 0);
        // eslint-disable-next-line no-bitwise
        out.push(((v4[2] << 8) | v4[3]) >>> 0);
        continue;
      }
      const n = Number.parseInt(part, 16);
      if (!Number.isFinite(n) || n < 0 || n > 0xffff) {
        throw new Error(`Invalid IPv6 hextet: ${part}`);
      }
      out.push(n);
    }
    return out;
  };

  const leftHextets = parseParts(leftParts);
  const rightHextets = parseParts(rightParts);

  /** @type {number[]} */
  let hextets;
  if (right !== null) {
    const missing = 8 - (leftHextets.length + rightHextets.length);
    if (missing < 0) {
      throw new Error(`Invalid IPv6 address: ${address}`);
    }
    hextets = [...leftHextets, ...Array(missing).fill(0), ...rightHextets];
  } else {
    if (leftHextets.length !== 8) {
      throw new Error(`Invalid IPv6 address: ${address}`);
    }
    hextets = leftHextets;
  }

  if (hextets.length !== 8) {
    throw new Error(`Invalid IPv6 address: ${address}`);
  }

  const bytes = new Uint8Array(16);
  for (let i = 0; i < 8; i++) {
    const v = hextets[i];
    // eslint-disable-next-line no-bitwise
    bytes[i * 2] = (v >> 8) & 0xff;
    // eslint-disable-next-line no-bitwise
    bytes[i * 2 + 1] = v & 0xff;
  }
  return bytes;
}

function ipv6IsIpv4Mapped(bytes) {
  for (let i = 0; i < 10; i++) {
    if (bytes[i] !== 0) return false;
  }
  return bytes[10] === 0xff && bytes[11] === 0xff;
}

function isPrivateIpv6(bytes) {
  // ::/128 (unspecified)
  if (bytes.every((b) => b === 0)) return true;

  // ::1/128 (loopback)
  let loopback = true;
  for (let i = 0; i < 15; i++) loopback = loopback && bytes[i] === 0;
  if (loopback && bytes[15] === 1) return true;

  // ff00::/8 (multicast)
  if (bytes[0] === 0xff) return true;

  // fc00::/7 (unique local)
  // eslint-disable-next-line no-bitwise
  if ((bytes[0] & 0xfe) === 0xfc) return true;

  // fe80::/10 (link-local)
  // eslint-disable-next-line no-bitwise
  if (bytes[0] === 0xfe && (bytes[1] & 0xc0) === 0x80) return true;

  // 2001:db8::/32 (documentation)
  if (bytes[0] === 0x20 && bytes[1] === 0x01 && bytes[2] === 0x0d && bytes[3] === 0xb8) return true;

  return false;
}

function isAllowedIpAddress(ipStr, allowCidrs, allowPrivateIps) {
  const family = net.isIP(ipStr);
  if (family === 4) {
    const bytes = ipv4StringToBytes(ipStr);
    const explicitlyAllowed = allowCidrs.some((cidr) => ipv4InCidr(bytes, cidr));
    if (allowPrivateIps) return true;
    if (!isPrivateIpv4(bytes)) return true;
    return explicitlyAllowed;
  }

  if (family === 6) {
    if (allowPrivateIps) return true;
    const bytes = parseIpv6ToBytes(ipStr);
    if (ipv6IsIpv4Mapped(bytes)) {
      const v4 = new Uint8Array(bytes.subarray(12, 16));
      const explicitlyAllowed = allowCidrs.some((cidr) => ipv4InCidr(v4, cidr));
      if (!isPrivateIpv4(v4)) return true;
      return explicitlyAllowed;
    }
    return !isPrivateIpv6(bytes);
  }

  return false;
}

class TcpMuxIpPolicyDeniedError extends Error {
  constructor(message) {
    super(message);
    this.name = "TcpMuxIpPolicyDeniedError";
  }
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

function normalizeOpenHost(host) {
  if (typeof host !== "string") return "";
  let out = host.trim();
  if (out.startsWith("[") && out.endsWith("]")) out = out.slice(1, -1);
  return out;
}

function asBuffer(data) {
  if (Buffer.isBuffer(data)) return data;
  if (data instanceof ArrayBuffer) return Buffer.from(data);
  if (ArrayBuffer.isView(data)) return Buffer.from(data.buffer, data.byteOffset, data.byteLength);
  const t = data === null ? "null" : typeof data;
  throw new TypeError(`Unsupported WebSocket message payload type: ${t}`);
}

export async function createProxyServer(userConfig) {
  const config = {
    host: "127.0.0.1",
    port: 8080,
    path: "/tcp-mux",
    authToken: undefined,
    allowPrivateIps: false,
    // IPv4-only allowlist for local development. If an IP is in this list, it is
    // allowed even if it is in a private/reserved range.
    allowCidrs: [],
    maxOpenConnectionsPerSession: 256,
    maxStreamBufferedBytes: 1024 * 1024,
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
  };

  const log = config.logger ?? defaultLogger;

  const httpServer = http.createServer((req, res) => {
    res.statusCode = 404;
    res.end();
  });

  const wss = new WebSocketServer({
    noServer: true,
    // `ws` only enforces this per WebSocket *message*; `aero-tcp-mux-v1` frames
    // may be split across messages, so we separately cap the pending parser
    // buffer size below.
    maxPayload: config.maxFramePayloadBytes + TCP_MUX_HEADER_BYTES + 64,
    handleProtocols: (protocols) => (protocols.has(TCP_MUX_SUBPROTOCOL) ? TCP_MUX_SUBPROTOCOL : false),
  });

  function rejectUpgrade(socket, statusCode, message) {
    const safeMessage = formatOneLineUtf8(message, MAX_UPGRADE_STATUS_MESSAGE_BYTES) || "Error";
    try {
      socket.write(`HTTP/1.1 ${statusCode} ${safeMessage}\r\n\r\n`);
    } catch {
      // ignore
    }
    try {
      socket.destroy();
    } catch {
      // ignore
    }
  }

  httpServer.on("upgrade", (req, socket, head) => {
    try {
      const rawUrl = req.url ?? "/";
      if (typeof rawUrl !== "string") {
        rejectUpgrade(socket, 400, "Bad Request");
        return;
      }
      if (rawUrl.length > MAX_REQUEST_URL_LEN) {
        rejectUpgrade(socket, 414, "URI Too Long");
        return;
      }

      let url;
      try {
        url = new URL(rawUrl, "http://localhost");
      } catch {
        rejectUpgrade(socket, 400, "Bad Request");
        return;
      }
      if (url.pathname !== config.path) {
        rejectUpgrade(socket, 404, "Not Found");
        return;
      }

      const protocolHeaderRaw = req.headers["sec-websocket-protocol"];
      const protocolHeader =
        typeof protocolHeaderRaw === "string" || Array.isArray(protocolHeaderRaw) ? protocolHeaderRaw : undefined;
      const subprotocol = hasWebSocketSubprotocol(protocolHeader, TCP_MUX_SUBPROTOCOL);
      if (!subprotocol.ok) {
        rejectUpgrade(socket, 400, "Invalid Sec-WebSocket-Protocol header");
        return;
      }
      if (!subprotocol.has) {
        rejectUpgrade(socket, 400, `Missing required subprotocol: ${TCP_MUX_SUBPROTOCOL}`);
        return;
      }

      const tokenFromQuery = url.searchParams.get("token");
      if (tokenFromQuery && tokenFromQuery.length > MAX_TOKEN_LEN) {
        stats.authFailed += 1;
        rejectUpgrade(socket, 401, "Unauthorized");
        return;
      }

      const authHeader = singleHeaderValue(req.headers.authorization);
      if (authHeader === null) {
        stats.authFailed += 1;
        rejectUpgrade(socket, 401, "Unauthorized");
        return;
      }
      if (authHeader && authHeader.length > MAX_AUTH_HEADER_LEN) {
        stats.authFailed += 1;
        rejectUpgrade(socket, 401, "Unauthorized");
        return;
      }
      const tokenFromHeader = authHeader?.startsWith("Bearer ") ? authHeader.slice("Bearer ".length) : undefined;
      if (tokenFromHeader && tokenFromHeader.length > MAX_TOKEN_LEN) {
        stats.authFailed += 1;
        rejectUpgrade(socket, 401, "Unauthorized");
        return;
      }
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

    log({ level: "info", event: "ws_connected", sessionId, remoteIp, protocol: ws.protocol });

    const openBucket = new TokenBucket({
      refillPerSecond: config.maxOpenRequestsPerMinute / 60,
      burst: config.maxOpenRequestsPerMinute,
    });

    const bytesBucket = new TokenBucket({
      refillPerSecond: config.maxClientBytesPerSecond,
      burst: config.maxClientBytesPerSecond * 2,
    });

    /** @type {Map<number, { socket: net.Socket, connected: boolean, clientFin: boolean, endSent: boolean, serverFin: boolean, pendingWrites: Buffer[], pendingWriteBytes: number, writePaused: boolean, pausedForWsBackpressure: boolean }>} */
    const streams = new Map();

    const muxParser = new TcpMuxFrameParser(config.maxFramePayloadBytes);

    /** @type {Buffer[]} */
    const wsSendQueue = [];
    let wsSendQueueBytes = 0;
    let wsSendFlushScheduled = false;
    let wsBackpressureActive = false;
    /** @type {ReturnType<typeof setTimeout> | null} */
    let wsBackpressurePollTimer = null;

    function backlogBytes() {
      // We track the queue we control (wsSendQueueBytes) and also include the
      // ws library's own internal buffer measurement for extra safety.
      return wsSendQueueBytes + (ws.bufferedAmount ?? 0);
    }

    function scheduleWsBackpressurePoll() {
      if (wsBackpressurePollTimer) return;
      wsBackpressurePollTimer = setTimeout(() => {
        wsBackpressurePollTimer = null;
        if (ws.readyState !== WebSocket.OPEN) return;
        maybeResumeTcpForWsBackpressure();
        if (wsBackpressureActive) scheduleWsBackpressurePoll();
      }, 10);
      wsBackpressurePollTimer.unref?.();
    }

    function maybePauseTcpForWsBackpressure() {
      if (ws.readyState !== WebSocket.OPEN) return;
      if (wsBackpressureActive) return;
      if (backlogBytes() <= config.wsBackpressureHighWatermarkBytes) return;

      let didPauseAny = false;
      for (const stream of streams.values()) {
        if (stream.pausedForWsBackpressure) continue;
        stream.socket.pause();
        stream.pausedForWsBackpressure = true;
        didPauseAny = true;
      }

      if (!didPauseAny) return;
      wsBackpressureActive = true;
      stats.wsBackpressurePauses += 1;
      scheduleWsBackpressurePoll();
    }

    function maybeResumeTcpForWsBackpressure() {
      if (!wsBackpressureActive) return;
      if (ws.readyState !== WebSocket.OPEN) return;
      if (backlogBytes() > config.wsBackpressureLowWatermarkBytes) return;

      let didResumeAny = false;
      for (const stream of streams.values()) {
        if (!stream.pausedForWsBackpressure) continue;
        stream.socket.resume();
        stream.pausedForWsBackpressure = false;
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
        try {
          ws.send(frame, { binary: true }, () => {});
        } catch (err) {
          log({
            level: "warn",
            event: "ws_send_failed",
            sessionId,
            message: formatOneLineError(err, 512, "Error"),
          });
          try {
            ws.terminate();
          } catch {
            // ignore
          }
          return;
        }
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

    function sendFrame(frame) {
      if (ws.readyState !== WebSocket.OPEN) return;
      stats.framesToClient += 1;
      stats.bytesToClient += frame.byteLength;
      wsSendQueue.push(frame);
      wsSendQueueBytes += frame.byteLength;
      maybePauseTcpForWsBackpressure();
      scheduleWsFlush();
    }

    function sendMuxFrame(msgType, streamId, payload) {
      sendFrame(encodeTcpMuxFrame(msgType, streamId, payload));
    }

    function sendStreamError(streamId, code, message) {
      sendMuxFrame(
        TcpMuxMsgType.ERROR,
        streamId,
        encodeTcpMuxErrorPayload(code, formatTcpMuxErrorMessage(message)),
      );
    }

    function removeStream(streamId) {
      const stream = streams.get(streamId);
      if (!stream) return null;
      streams.delete(streamId);
      stats.tcpConnectionsActive = Math.max(0, stats.tcpConnectionsActive - 1);
      return stream;
    }

    function destroyStream(streamId) {
      const stream = removeStream(streamId);
      if (!stream) return;
      try {
        stream.socket.removeAllListeners();
        stream.socket.destroy();
      } catch {
        // ignore
      }
    }

    function enqueueStreamWrite(streamId, stream, data) {
      stream.pendingWrites.push(data);
      stream.pendingWriteBytes += data.length;
      if (stream.pendingWriteBytes <= config.maxStreamBufferedBytes) return;

      sendStreamError(streamId, TcpMuxErrorCode.STREAM_BUFFER_OVERFLOW, "stream buffered too much data");
      destroyStream(streamId);
    }

    function flushStreamWrites(streamId, stream) {
      if (ws.readyState !== WebSocket.OPEN) return;
      if (!stream.connected) return;
      if (stream.writePaused) return;

      while (stream.pendingWrites.length > 0) {
        const chunk = stream.pendingWrites.shift();
        stream.pendingWriteBytes -= chunk.length;
        let ok = false;
        try {
          ok = stream.socket.write(chunk);
        } catch (err) {
          log({
            level: "warn",
            event: "tcp_write_failed",
            sessionId,
            streamId,
            message: formatOneLineError(err, 512, "Error"),
          });
          sendStreamError(streamId, TcpMuxErrorCode.DIAL_FAILED, "dial failed");
          destroyStream(streamId);
          return;
        }
        if (!ok) {
          stream.writePaused = true;
          return;
        }
      }

      // If the client already sent FIN, send it to the TCP socket only after all
      // buffered writes have been flushed. This allows clients to send
      // OPEN+DATA+FIN in a single WebSocket message without losing the DATA.
      if (stream.clientFin && !stream.endSent) {
        stream.endSent = true;
        try {
          stream.socket.end();
        } catch (err) {
          log({
            level: "warn",
            event: "tcp_end_failed",
            sessionId,
            streamId,
            message: formatOneLineError(err, 512, "Error"),
          });
          destroyStream(streamId);
          return;
        }
      }

      if (stream.clientFin && stream.serverFin) {
        // Both halves have finished; release resources early.
        destroyStream(streamId);
      }
    }

    function handleOpen(frame) {
      if (frame.streamId === 0) {
        sendStreamError(frame.streamId, TcpMuxErrorCode.PROTOCOL_ERROR, "stream_id=0 is reserved");
        return;
      }
      if (streams.has(frame.streamId)) {
        sendStreamError(frame.streamId, TcpMuxErrorCode.PROTOCOL_ERROR, "stream_id already exists");
        return;
      }
      if (!openBucket.tryConsume(1)) {
        stats.rateLimited += 1;
        sendStreamError(frame.streamId, TcpMuxErrorCode.STREAM_LIMIT_EXCEEDED, "OPEN rate limit");
        return;
      }
      if (streams.size >= config.maxOpenConnectionsPerSession) {
        sendStreamError(frame.streamId, TcpMuxErrorCode.STREAM_LIMIT_EXCEEDED, "Too many open streams");
        return;
      }

      let open;
      try {
        open = decodeTcpMuxOpenPayload(frame.payload);
      } catch (err) {
        // Keep client-visible error strings stable; do not reflect parser exception messages.
        sendStreamError(frame.streamId, TcpMuxErrorCode.PROTOCOL_ERROR, "Invalid OPEN payload");
        return;
      }

      const host = normalizeOpenHost(open.host);
      const port = open.port;
      if (host.length === 0) {
        sendStreamError(frame.streamId, TcpMuxErrorCode.PROTOCOL_ERROR, "host is required");
        return;
      }
      if (!Number.isInteger(port) || port < 1 || port > 65535) {
        sendStreamError(frame.streamId, TcpMuxErrorCode.PROTOCOL_ERROR, "invalid port");
        return;
      }

      const hostFamily = net.isIP(host);

      if (hostFamily !== 0) {
        const allowed = isAllowedIpAddress(host, allowCidrs, config.allowPrivateIps);
        if (!allowed) {
          stats.deniedDestinations += 1;
          log({ level: "warn", event: "policy_denied", sessionId, streamId: frame.streamId, host, port });
          sendStreamError(frame.streamId, TcpMuxErrorCode.POLICY_DENIED, "Target IP is blocked by IP egress policy");
          return;
        }
      }

      let dialHost = host;
      /** @type {undefined | ((hostname: string, options: any, cb: (err: Error | null, address: string, family: number) => void) => void)} */
      let dialLookup;

      if (hostFamily === 0) {
        dialLookup = (_hostname, _options, cb) => {
          void (async () => {
            let addresses;
            try {
              addresses = await lookup(dialHost, { all: true, verbatim: true });
            } catch (err) {
              cb(err, "", 4);
              return;
            }

            for (const { address, family } of addresses) {
              if (isAllowedIpAddress(address, allowCidrs, config.allowPrivateIps)) {
                cb(null, address, family);
                return;
              }
            }

            cb(new TcpMuxIpPolicyDeniedError("Target IP is blocked by IP egress policy"), "", 4);
          })();
        };
      }

      stats.tcpConnectionsTotal += 1;
      stats.tcpConnectionsActive += 1;

      log({ level: "info", event: "tcp_connect_start", sessionId, streamId: frame.streamId, host, port });

      const socket = net.createConnection({ host: dialHost, port, allowHalfOpen: true, lookup: dialLookup });
      socket.setNoDelay(true);

      const stream = {
        socket,
        connected: false,
        clientFin: false,
        endSent: false,
        serverFin: false,
        pendingWrites: [],
        pendingWriteBytes: 0,
        writePaused: false,
        pausedForWsBackpressure: false,
      };
      streams.set(frame.streamId, stream);

      if (wsBackpressureActive) {
        stream.pausedForWsBackpressure = true;
        socket.pause();
      }

      socket.on("connect", () => {
        stream.connected = true;
        log({ level: "info", event: "tcp_connected", sessionId, streamId: frame.streamId, host, port });
        flushStreamWrites(frame.streamId, stream);
      });

      socket.on("data", (chunk) => {
        sendMuxFrame(TcpMuxMsgType.DATA, frame.streamId, chunk);
      });

      socket.on("drain", () => {
        stream.writePaused = false;
        flushStreamWrites(frame.streamId, stream);
      });

      socket.on("end", () => {
        if (stream.serverFin) return;
        stream.serverFin = true;
        sendMuxFrame(TcpMuxMsgType.CLOSE, frame.streamId, encodeTcpMuxClosePayload(TcpMuxCloseFlags.FIN));
        flushStreamWrites(frame.streamId, stream);
      });

      socket.on("error", (err) => {
        log({
          level: "warn",
          event: "tcp_error",
          sessionId,
          streamId: frame.streamId,
          host,
          port,
          message: formatTcpMuxErrorMessage(err),
        });

        if (err instanceof TcpMuxIpPolicyDeniedError) {
          stats.deniedDestinations += 1;
          // Keep client-visible error strings stable; do not reflect the exception message.
          sendStreamError(frame.streamId, TcpMuxErrorCode.POLICY_DENIED, "Target IP is blocked by IP egress policy");
        } else {
          sendStreamError(frame.streamId, TcpMuxErrorCode.DIAL_FAILED, "dial failed");
        }
        destroyStream(frame.streamId);
      });

      socket.on("close", () => {
        removeStream(frame.streamId);
        log({ level: "info", event: "tcp_closed", sessionId, streamId: frame.streamId });
      });
    }

    function handleData(frame) {
      const stream = streams.get(frame.streamId);
      if (!stream) {
        sendStreamError(frame.streamId, TcpMuxErrorCode.UNKNOWN_STREAM, "unknown stream");
        return;
      }
      if (stream.clientFin) {
        sendStreamError(frame.streamId, TcpMuxErrorCode.PROTOCOL_ERROR, "stream is half-closed (client FIN)");
        return;
      }

      const payloadLen = frame.payload.length;
      if (!bytesBucket.tryConsume(payloadLen)) {
        stats.rateLimited += 1;
        sendStreamError(frame.streamId, TcpMuxErrorCode.STREAM_BUFFER_OVERFLOW, "DATA rate limit");
        destroyStream(frame.streamId);
        return;
      }

      if (!stream.connected || stream.writePaused) {
        enqueueStreamWrite(frame.streamId, stream, frame.payload);
        return;
      }

      let ok = false;
      try {
        ok = stream.socket.write(frame.payload);
      } catch (err) {
        log({
          level: "warn",
          event: "tcp_write_failed",
          sessionId,
          streamId: frame.streamId,
          message: formatOneLineError(err, 512, "Error"),
        });
        sendStreamError(frame.streamId, TcpMuxErrorCode.DIAL_FAILED, "dial failed");
        destroyStream(frame.streamId);
        return;
      }
      if (!ok) {
        stream.writePaused = true;
      }
    }

    function handleClose(frame) {
      const stream = streams.get(frame.streamId);
      if (!stream) return;

      let flags;
      try {
        flags = decodeTcpMuxClosePayload(frame.payload).flags;
      } catch (err) {
        // Keep client-visible error strings stable; do not reflect parser exception messages.
        sendStreamError(frame.streamId, TcpMuxErrorCode.PROTOCOL_ERROR, "Invalid CLOSE payload");
        return;
      }

      if ((flags & TcpMuxCloseFlags.RST) !== 0) {
        destroyStream(frame.streamId);
        return;
      }

      if ((flags & TcpMuxCloseFlags.FIN) !== 0) {
        stream.clientFin = true;
        flushStreamWrites(frame.streamId, stream);
      }
    }

    function handleMuxFrame(frame) {
      if (frame.payload.length > config.maxFramePayloadBytes) {
        wsCloseSafe(ws, 1002, "Frame payload too large");
        return;
      }

      switch (frame.msgType) {
        case TcpMuxMsgType.OPEN:
          handleOpen(frame);
          return;
        case TcpMuxMsgType.DATA:
          handleData(frame);
          return;
        case TcpMuxMsgType.CLOSE:
          handleClose(frame);
          return;
        case TcpMuxMsgType.ERROR:
          // Clients should not send ERROR frames (v1); ignore.
          return;
        case TcpMuxMsgType.PING:
          sendMuxFrame(TcpMuxMsgType.PONG, frame.streamId, frame.payload);
          return;
        case TcpMuxMsgType.PONG:
          return;
        default:
          sendStreamError(frame.streamId, TcpMuxErrorCode.PROTOCOL_ERROR, `Unknown msg_type ${frame.msgType}`);
      }
    }

    ws.on("message", (data, isBinary) => {
      if (!isBinary) {
        // /tcp-mux is a binary protocol; close with "unsupported data".
        wsCloseSafe(ws, 1003, "Binary messages only");
        return;
      }

      let chunk;
      try {
        chunk = asBuffer(data);
      } catch (err) {
        wsCloseSafe(ws, 1002, "Protocol error");
        return;
      }

      stats.bytesFromClient += chunk.length;

      let frames;
      try {
        frames = muxParser.push(chunk);
      } catch (err) {
        wsCloseSafe(ws, 1002, "Protocol error");
        return;
      }
      stats.framesFromClient += frames.length;
      for (const frame of frames) {
        handleMuxFrame(frame);
      }

      if (muxParser.pendingBytes() > TCP_MUX_HEADER_BYTES + config.maxFramePayloadBytes) {
        wsCloseSafe(ws, 1002, "Framing buffer overflow");
      }
    });

    ws.on("close", () => {
      stats.wsConnectionsActive = Math.max(0, stats.wsConnectionsActive - 1);
      log({ level: "info", event: "ws_closed", sessionId });
      if (wsBackpressurePollTimer) {
        clearTimeout(wsBackpressurePollTimer);
        wsBackpressurePollTimer = null;
      }
      for (const id of streams.keys()) destroyStream(id);
    });

    ws.on("error", (err) => {
      log({ level: "warn", event: "ws_error", sessionId, message: formatTcpMuxErrorMessage(err) });
    });
  });

  let metricsTimer = null;
  if (config.metricsIntervalMs > 0) {
    metricsTimer = setInterval(() => {
      log({ level: "info", event: "metrics", ...stats });
    }, config.metricsIntervalMs);
    metricsTimer.unref?.();
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
