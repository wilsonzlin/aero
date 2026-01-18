import http from "node:http";
import { WebSocketServer } from "ws";
import { createHttpHandler } from "./http.js";
import { createLogger } from "./logger.js";
import { createMetrics } from "./metrics.js";
import { getAuthTokenFromRequest, isOriginAllowed, isTokenAllowed } from "./auth.js";
import { TcpProxyManager } from "./tcpProxy.js";
import { rejectHttpUpgrade } from "../../src/http_upgrade_reject.js";
import { formatOneLineError } from "./text.js";
import { tryGetProp, tryGetStringProp } from "../../src/safe_props.js";

const MAX_UPGRADE_URL_LEN = 8 * 1024;

function getClientIpFromWsRequest(ws, req) {
  const ipFromWs = tryGetStringProp(ws, "_aeroClientIp");
  if (ipFromWs) return ipFromWs;
  const socket = tryGetProp(req, "socket");
  return tryGetStringProp(socket, "remoteAddress") ?? "unknown";
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
    const ip = getClientIpFromWsRequest(ws, req);
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
    try {
      const rawUrl = req.url ?? "/";
      if (typeof rawUrl !== "string") {
        rejectHttpUpgrade(socket, 400, "Bad Request");
        return;
      }
      if (rawUrl.length > MAX_UPGRADE_URL_LEN) {
        rejectHttpUpgrade(socket, 414, "URI Too Long");
        return;
      }

      let url;
      try {
        url = new URL(rawUrl, "http://localhost");
      } catch {
        rejectHttpUpgrade(socket, 400, "Bad Request");
        return;
      }
      if (url.pathname !== "/ws/tcp") {
        rejectHttpUpgrade(socket, 404, "Not Found");
        return;
      }

      if (!isOriginAllowed(req.headers.origin, config.allowedOrigins)) {
        rejectHttpUpgrade(socket, 403, "Forbidden");
        return;
      }

      const token = getAuthTokenFromRequest(req, url.searchParams);
      if (!isTokenAllowed(token, config.tokens)) {
        rejectHttpUpgrade(socket, 401, "Unauthorized");
        return;
      }

      const ip = getClientIpFromWsRequest(undefined, req);
      const current = wsConnectionsByIp.get(ip) ?? 0;
      if (current >= config.maxWsConnectionsPerIp) {
        rejectHttpUpgrade(socket, 429, "Too Many Requests");
        return;
      }

      try {
        wss.handleUpgrade(req, socket, head, (ws) => {
          ws._aeroClientIp = ip;
          ws._aeroAuthToken = token;
          wss.emit("connection", ws, req);
        });
      } catch (err) {
        logger.warn("ws_upgrade_failed", { err: formatOneLineError(err, 512) });
        rejectHttpUpgrade(socket, 500, "WebSocket upgrade failed");
      }
    } catch (err) {
      logger.warn("ws_upgrade_failed", { err: formatOneLineError(err, 512) });
      rejectHttpUpgrade(socket, 500, "WebSocket upgrade failed");
    }
  });

  return { httpServer, wss, logger, metrics };
}

