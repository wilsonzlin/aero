import net from "node:net";
import { TokenBucket } from "./rateLimit.js";
import {
  CloseReason,
  OpenStatus,
  decodeClientFrame,
  encodeOpenedFrame,
  encodeServerCloseFrame,
  encodeServerDataFrame,
  encodeServerEndFrame,
} from "./protocol.js";
import { PolicyError, resolveAndValidateTarget } from "./policy.js";
import { formatOneLineError, formatOneLineUtf8 } from "./text.js";

const WS_CLOSE_POLICY_VIOLATION = 1008;
const WS_CLOSE_UNSUPPORTED_DATA = 1003;

let nextSessionId = 1;

const MAX_WS_CLOSE_REASON_BYTES = 123;

function closeWebSocket(ws, code, reason) {
  try {
    ws.close(code, formatOneLineUtf8(reason, MAX_WS_CLOSE_REASON_BYTES));
  } catch {
    // ignore
  }
}

export class TcpProxyManager {
  constructor({ config, logger, metrics }) {
    this.config = config;
    this.logger = logger;
    this.metrics = metrics;
  }

  handleWebSocket(ws, req) {
    const sessionId = nextSessionId++;
    const clientIp = ws._aeroClientIp ?? req.socket.remoteAddress ?? "unknown";
    const session = new TcpProxySession({
      sessionId,
      ws,
      config: this.config,
      logger: this.logger,
      metrics: this.metrics,
      clientIp,
    });
    session.start();
  }
}

class TcpProxySession {
  constructor({ sessionId, ws, config, logger, metrics, clientIp }) {
    this.sessionId = sessionId;
    this.ws = ws;
    this.config = config;
    this.logger = logger;
    this.metrics = metrics;
    this.clientIp = clientIp;

    this.connections = new Map(); // connId -> net.Socket
    this.connectAttempts = [];

    const burst = Math.max(1, this.config.bandwidthBytesPerSecond) * 2;
    this.inBucket = new TokenBucket({ capacity: burst, refillPerSecond: this.config.bandwidthBytesPerSecond });
    this.outBucket = new TokenBucket({ capacity: burst, refillPerSecond: this.config.bandwidthBytesPerSecond });
  }

  start() {
    this.logger.info("ws_connected", { sessionId: this.sessionId, clientIp: this.clientIp });

    this.ws.on("message", (data, isBinary) => {
      if (!isBinary) {
        closeWebSocket(this.ws, WS_CLOSE_UNSUPPORTED_DATA, "Binary frames required");
        return;
      }
      const buf = Buffer.isBuffer(data) ? data : Buffer.from(data);
      if (!this.inBucket.tryRemove(buf.length)) {
        this.logger.warn("rate_limited_in", { sessionId: this.sessionId, clientIp: this.clientIp });
        closeWebSocket(this.ws, WS_CLOSE_POLICY_VIOLATION, "Rate limited");
        return;
      }
      this.metrics.increment("bytesInTotal", buf.length);
      this.#handleFrame(buf).catch((err) => {
        this.logger.warn("ws_frame_error", { sessionId: this.sessionId, err: formatOneLineError(err, 512) });
        closeWebSocket(this.ws, WS_CLOSE_POLICY_VIOLATION, "Protocol error");
      });
    });

    this.ws.once("close", () => {
      this.logger.info("ws_closed", { sessionId: this.sessionId, clientIp: this.clientIp });
      for (const conn of this.connections.values()) conn.socket.destroy();
      this.connections.clear();
    });
  }

  #send(buf) {
    if (!this.outBucket.tryRemove(buf.length)) {
      this.logger.warn("rate_limited_out", { sessionId: this.sessionId, clientIp: this.clientIp });
      closeWebSocket(this.ws, WS_CLOSE_POLICY_VIOLATION, "Rate limited");
      return;
    }
    this.metrics.increment("bytesOutTotal", buf.length);
    this.ws.send(buf, { binary: true });
  }

  #sendOpened(connId, status, message = "") {
    this.#send(encodeOpenedFrame({ connId, status, message }));
  }

  #sendClose(connId, reason, message = "") {
    this.#send(encodeServerCloseFrame({ connId, reason, message }));
  }

  #isConnectRateLimited() {
    const now = Date.now();
    this.connectAttempts = this.connectAttempts.filter((t) => now - t < 60_000);
    if (this.connectAttempts.length >= this.config.connectsPerMinute) return true;
    this.connectAttempts.push(now);
    return false;
  }

  async #handleFrame(buf) {
    const frame = decodeClientFrame(buf);

    if (frame.type === "connect") {
      if (this.#isConnectRateLimited()) {
        this.metrics.increment("tcpRejectedTotal");
        this.#sendOpened(frame.connId, OpenStatus.LIMIT, "Too many connect attempts");
        return;
      }
      await this.#handleConnect(frame);
      return;
    }
    if (frame.type === "data") {
      this.#handleClientData(frame.connId, frame.data);
      return;
    }
    if (frame.type === "end") {
      this.#handleClientEnd(frame.connId);
      return;
    }
    if (frame.type === "close") {
      this.#handleClientClose(frame.connId);
      return;
    }
  }

  async #handleConnect({ connId, host, port }) {
    if (this.connections.has(connId)) {
      this.metrics.increment("tcpRejectedTotal");
      this.#sendOpened(connId, OpenStatus.PROTOCOL, "connId already in use");
      return;
    }
    if (this.connections.size >= this.config.maxTcpConnectionsPerWs) {
      this.metrics.increment("tcpRejectedTotal");
      this.#sendOpened(connId, OpenStatus.LIMIT, "Too many TCP connections for this client");
      return;
    }
    try {
      const { address, family } = await resolveAndValidateTarget({ host, port }, this.config);
      this.logger.info("tcp_connect_attempt", {
        sessionId: this.sessionId,
        connId,
        host,
        port,
        address,
        family,
      });

      // Global cap is enforced via tcpConnectionsCurrent gauge in metrics (best-effort).
      // To keep enforcement deterministic, also track in-process.
      if (TcpProxySession._tcpTotal >= this.config.maxTcpConnectionsTotal) {
        this.metrics.increment("tcpRejectedTotal");
        this.#sendOpened(connId, OpenStatus.LIMIT, "Server connection limit reached");
        return;
      }

      const socket = net.createConnection({ host: address, port, family, allowHalfOpen: true });
      socket.setNoDelay(true);

      const conn = new ProxyTcpConnection({ connId, socket });
      this.connections.set(connId, conn);
      TcpProxySession._tcpTotal += 1;
      this.metrics.increment("tcpConnectionsTotal");
      this.metrics.addGauge("tcpConnectionsCurrent", 1);

      socket.once("connect", () => {
        conn.openResponseSent = true;
        conn.openOk = true;
        this.#sendOpened(connId, OpenStatus.OK, "");
      });

      socket.on("data", (chunk) => {
        if (this.ws.readyState !== this.ws.OPEN) return;
        this.#send(encodeServerDataFrame({ connId, data: chunk }));
      });

      socket.on("end", () => {
        if (this.ws.readyState !== this.ws.OPEN) return;
        this.#send(encodeServerEndFrame({ connId }));
      });

      socket.once("error", (err) => {
        const clientMessage = formatConnectErrorForClient(err);
        conn.lastErrorMessage = clientMessage;
        this.logger.warn("tcp_socket_error", {
          sessionId: this.sessionId,
          connId,
          code: typeof err?.code === "string" ? err.code : undefined,
        });
        if (!conn.openResponseSent && this.ws.readyState === this.ws.OPEN) {
          conn.openResponseSent = true;
          this.#sendOpened(connId, OpenStatus.CONNECT, clientMessage);
        }
      });

      socket.once("close", (hadError) => {
        this.connections.delete(connId);
        TcpProxySession._tcpTotal = Math.max(0, TcpProxySession._tcpTotal - 1);
        this.metrics.addGauge("tcpConnectionsCurrent", -1);
        if (this.ws.readyState === this.ws.OPEN && conn.openOk) {
          const message = conn.lastErrorMessage ?? "";
          this.#sendClose(connId, hadError ? CloseReason.ERROR : CloseReason.REMOTE_CLOSED, message);
        }
      });
    } catch (err) {
      this.metrics.increment("tcpRejectedTotal");
      if (err instanceof PolicyError) {
        // Keep policy failures stable and non-leaky.
        this.#sendOpened(connId, OpenStatus.POLICY, "Target is not allowed");
        return;
      }
      this.#sendOpened(connId, OpenStatus.CONNECT, formatConnectErrorForClient(err));
    }
  }

  #handleClientData(connId, data) {
    const conn = this.connections.get(connId);
    if (!conn) {
      if (this.ws.readyState === this.ws.OPEN) this.#sendClose(connId, CloseReason.PROTOCOL, "Unknown connId");
      return;
    }
    conn.socket.write(data);
  }

  #handleClientEnd(connId) {
    const conn = this.connections.get(connId);
    if (!conn) return;
    conn.socket.end();
  }

  #handleClientClose(connId) {
    const conn = this.connections.get(connId);
    if (!conn) return;
    conn.socket.destroy();
  }
}

TcpProxySession._tcpTotal = 0;

class ProxyTcpConnection {
  constructor({ connId, socket }) {
    this.connId = connId;
    this.socket = socket;
    this.openResponseSent = false;
    this.openOk = false;
    this.lastErrorMessage = null;
  }
}

function formatConnectErrorForClient(err) {
  const code = typeof err?.code === "string" ? err.code : "";
  switch (code) {
    case "ECONNREFUSED":
      return "connection refused";
    case "ETIMEDOUT":
      return "connection timed out";
    case "EHOSTUNREACH":
    case "ENETUNREACH":
      return "host unreachable";
    default:
      return "connect failed";
  }
}
