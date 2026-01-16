import http from "node:http";
import { WebSocketServer } from "ws";
import { createHttpHandler } from "./http.js";
import { createLogger } from "./logger.js";
import { createMetrics } from "./metrics.js";
import { getAuthTokenFromRequest, isOriginAllowed, isTokenAllowed } from "./auth.js";
import { TcpProxyManager } from "./tcpProxy.js";
import { formatOneLineUtf8 } from "./text.js";

const MAX_UPGRADE_URL_LEN = 8 * 1024;
const MAX_UPGRADE_STATUS_MESSAGE_BYTES = 64;

function writeUpgradeResponse(socket, statusCode, statusMessage) {
  const safeStatusMessage = formatOneLineUtf8(statusMessage, MAX_UPGRADE_STATUS_MESSAGE_BYTES) || "Error";
  socket.write(`HTTP/1.1 ${statusCode} ${safeStatusMessage}\r\n\r\n`);
  socket.destroy();
}

export function createAeroServer(config, { logger = createLogger({ level: config.logLevel }), metrics = createMetrics() } = {}) {
  const tcpProxy = new TcpProxyManager({ config, logger, metrics });
  const httpHandler = createHttpHandler({ config, logger, metrics });

  const httpServer = http.createServer(httpHandler);

  const wss = new WebSocketServer({
    noServer: true,
    maxPayload: config.maxWsMessageBytes,
  });

  const wsConnectionsByIp = new Map();

  wss.on("connection", (ws, req) => {
    const ip = ws._aeroClientIp ?? req.socket.remoteAddress ?? "unknown";
    const current = wsConnectionsByIp.get(ip) ?? 0;
    wsConnectionsByIp.set(ip, current + 1);
    metrics.increment("wsConnectionsTotal");
    metrics.addGauge("wsConnectionsCurrent", 1);

    ws.once("close", () => {
      const cur = wsConnectionsByIp.get(ip) ?? 0;
      if (cur <= 1) wsConnectionsByIp.delete(ip);
      else wsConnectionsByIp.set(ip, cur - 1);
      metrics.addGauge("wsConnectionsCurrent", -1);
    });

    tcpProxy.handleWebSocket(ws, req);
  });

  httpServer.on("upgrade", (req, socket, head) => {
    const rawUrl = req.url ?? "/";
    if (typeof rawUrl !== "string") {
      writeUpgradeResponse(socket, 400, "Bad Request");
      return;
    }
    if (rawUrl.length > MAX_UPGRADE_URL_LEN) {
      writeUpgradeResponse(socket, 414, "URI Too Long");
      return;
    }

    let url;
    try {
      url = new URL(rawUrl, "http://localhost");
    } catch {
      writeUpgradeResponse(socket, 400, "Bad Request");
      return;
    }
    if (url.pathname !== "/ws/tcp") {
      writeUpgradeResponse(socket, 404, "Not Found");
      return;
    }

    if (!isOriginAllowed(req.headers.origin, config.allowedOrigins)) {
      writeUpgradeResponse(socket, 403, "Forbidden");
      return;
    }

    const token = getAuthTokenFromRequest(req, url.searchParams);
    if (!isTokenAllowed(token, config.tokens)) {
      writeUpgradeResponse(socket, 401, "Unauthorized");
      return;
    }

    const ip = req.socket.remoteAddress ?? "unknown";
    const current = wsConnectionsByIp.get(ip) ?? 0;
    if (current >= config.maxWsConnectionsPerIp) {
      writeUpgradeResponse(socket, 429, "Too Many Requests");
      return;
    }

    wss.handleUpgrade(req, socket, head, (ws) => {
      ws._aeroClientIp = ip;
      ws._aeroAuthToken = token;
      wss.emit("connection", ws, req);
    });
  });

  return { httpServer, wss, logger, metrics };
}

