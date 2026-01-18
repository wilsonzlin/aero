import http from "node:http";
import { WebSocketServer } from "ws";
import { loadConfigFromEnv, type ProxyConfig } from "./config";
import { setDohCorsHeaders } from "./cors";
import { handleDnsJson, handleDnsQuery } from "./dohHandlers";
import { formatError, log } from "./logger";
import { resolveAndAuthorizeTarget } from "./security";
import { parseTargetQuery } from "./targetQuery";
import { createProxyServerMetrics } from "./metrics";
import { wsCloseSafe } from "./wsClose";
import { validateWebSocketHandshakeRequest } from "./wsUpgradeRequest";
import { hasWebSocketSubprotocol } from "./wsSubprotocol";
import { rejectWsUpgrade } from "./wsUpgradeHttp";
import { handleTcpMuxRelay } from "./tcpMuxRelay";
import { handleTcpRelay } from "./tcpRelay";
import { handleUdpRelay, handleUdpRelayMultiplexed } from "./udpRelay";
import { TCP_MUX_SUBPROTOCOL } from "./tcpMuxProtocol";
import { tryGetProp, tryGetStringProp } from "../../src/safe_props.cjs";

export interface RunningProxyServer {
  server: http.Server;
  config: ProxyConfig;
  listenAddress: string;
  close: () => Promise<void>;
}

// Conservative cap to avoid spending unbounded CPU/memory on attacker-controlled request targets.
// Many HTTP stacks enforce ~8KB request target limits; keep this proxy strict and predictable.
const MAX_REQUEST_URL_LEN = 8 * 1024;

// DoH endpoints are implemented in `dohHandlers.ts`.

function sendJson(res: http.ServerResponse, statusCode: number, payload: unknown): void {
  // Defensive: encoding the response body must never throw. If JSON.stringify throws
  // (cyclic data, hostile toJSON, etc.), fall back to a stable 500 JSON body.
  let body: string;
  let code = statusCode;
  try {
    body = JSON.stringify(payload);
  } catch {
    code = 500;
    body = `{"error":"internal server error"}`;
  }

  try {
    res.writeHead(code, {
      "content-type": "application/json; charset=utf-8",
      "content-length": Buffer.byteLength(body),
      "cache-control": "no-store"
    });
    res.end(body);
  } catch {
    try {
      res.destroy();
    } catch {
      // ignore
    }
  }
}

export async function startProxyServer(overrides: Partial<ProxyConfig> = {}): Promise<RunningProxyServer> {
  const config: ProxyConfig = { ...loadConfigFromEnv(), ...overrides };
  const metrics = createProxyServerMetrics();
  const parsedUrlKey: unique symbol = Symbol("aero.parsedUrl");

  const server = http.createServer((req, res) => {
    void (async () => {
      const rawUrl = tryGetStringProp(req, "url");
      if (!rawUrl) {
        sendJson(res, 400, { error: "invalid url" });
        return;
      }
      if (rawUrl.length > MAX_REQUEST_URL_LEN) {
        sendJson(res, 414, { error: "url too long" });
        return;
      }

      let url: URL;
      try {
        url = new URL(rawUrl, "http://localhost");
      } catch {
        sendJson(res, 400, { error: "invalid url" });
        return;
      }

      if (req.method === "GET" && url.pathname === "/healthz") {
        sendJson(res, 200, { ok: true });
        return;
      }

      if (req.method === "GET" && url.pathname === "/metrics") {
        const body = metrics.prometheusText();
        try {
          res.writeHead(200, {
            "content-type": "text/plain; version=0.0.4; charset=utf-8",
            "content-length": Buffer.byteLength(body),
            "cache-control": "no-store"
          });
          res.end(body);
        } catch {
          try {
            res.destroy();
          } catch {
            // ignore
          }
        }
        return;
      }

      if (url.pathname === "/dns-query" || url.pathname === "/dns-json") {
        const allowMethods = url.pathname === "/dns-query" ? "GET, POST, OPTIONS" : "GET, OPTIONS";
        setDohCorsHeaders(req, res, config, { allowMethods });
        if (req.method === "OPTIONS") {
          try {
            res.writeHead(204);
            res.end();
          } catch {
            try {
              res.destroy();
            } catch {
              // ignore
            }
          }
          return;
        }
      }

      if (url.pathname === "/dns-query") {
        await handleDnsQuery(req, res, url, config);
        return;
      }

      if (req.method === "GET" && url.pathname === "/dns-json") {
        await handleDnsJson(req, res, url, config);
        return;
      }

      sendJson(res, 404, { error: "not found" });
    })().catch((err) => {
      log("error", "connect_error", { proto: "http", err: formatError(err) });
      // Defensive: avoid relying on response state getters (hostile/monkeypatched objects can
      // throw). `sendJson` is already resilient to writeHead/end throwing and will destroy the
      // response on failure.
      sendJson(res, 500, { error: "internal server error" });
    });
  });

  const wss = new WebSocketServer({ noServer: true, maxPayload: config.wsMaxPayloadBytes });
  const wssMux = new WebSocketServer({
    noServer: true,
    maxPayload: config.wsMaxPayloadBytes,
    handleProtocols: (protocols) => (protocols.has(TCP_MUX_SUBPROTOCOL) ? TCP_MUX_SUBPROTOCOL : false)
  });
  let nextConnId = 1;

  server.on("upgrade", (req, socket, head) => {
    try {
      const rawUrl = req.url;
      if (typeof rawUrl !== "string") {
        rejectWsUpgrade(socket, 400, "Invalid URL");
        return;
      }
      if (rawUrl.length > MAX_REQUEST_URL_LEN) {
        rejectWsUpgrade(socket, 414, "URL too long");
        return;
      }

      let url: URL;
      try {
        url = new URL(rawUrl, "http://localhost");
      } catch {
        rejectWsUpgrade(socket, 400, "Invalid URL");
        return;
      }

      if (url.pathname === "/tcp" || url.pathname === "/udp" || url.pathname === "/tcp-mux") {
        const decision = validateWebSocketHandshakeRequest(req);
        if (!decision.ok) {
          rejectWsUpgrade(socket, decision.status, decision.message);
          return;
        }
      }

      if (url.pathname === "/tcp-mux") {
        const protocolHeader = req.headers["sec-websocket-protocol"];
        const decision = hasWebSocketSubprotocol(
          typeof protocolHeader === "string" || Array.isArray(protocolHeader) ? protocolHeader : undefined,
          TCP_MUX_SUBPROTOCOL
        );
        if (!decision.ok) {
          rejectWsUpgrade(socket, 400, "Invalid Sec-WebSocket-Protocol header");
          return;
        }
        if (!decision.has) {
          rejectWsUpgrade(socket, 400, `Missing required subprotocol: ${TCP_MUX_SUBPROTOCOL}`);
          return;
        }

        try {
          wssMux.handleUpgrade(req, socket, head, (ws) => {
            wssMux.emit("connection", ws, req);
          });
        } catch (err) {
          metrics.incConnectionError("error");
          log("error", "connect_error", { proto: "tcp_mux", err: formatError(err) });
          rejectWsUpgrade(socket, 500, "WebSocket upgrade failed");
        }
        return;
      }

      if (url.pathname !== "/tcp" && url.pathname !== "/udp") {
        rejectWsUpgrade(socket, 404, "Not found");
        return;
      }

      (req as unknown as Record<symbol, unknown>)[parsedUrlKey] = url;
      try {
        wss.handleUpgrade(req, socket, head, (ws) => {
          wss.emit("connection", ws, req);
        });
      } catch (err) {
        metrics.incConnectionError("error");
        log("error", "connect_error", { proto: "ws_upgrade", err: formatError(err) });
        rejectWsUpgrade(socket, 500, "WebSocket upgrade failed");
      }
    } catch (err) {
      metrics.incConnectionError("error");
      log("error", "connect_error", { proto: "ws_upgrade", err: formatError(err) });
      rejectWsUpgrade(socket, 500, "WebSocket upgrade failed");
    }
  });

  wss.on("connection", (ws, req) => {
    const connId = nextConnId++;
    const storedUrl = (req as unknown as Record<symbol, unknown>)[parsedUrlKey];
    let parsedUrl: URL;
    if (storedUrl instanceof URL) {
      parsedUrl = storedUrl;
    } else {
      const rawUrl = tryGetStringProp(req, "url");
      if (!rawUrl) {
        wsCloseSafe(ws, 1002, "Invalid URL");
        return;
      }
      if (rawUrl.length > MAX_REQUEST_URL_LEN) {
        wsCloseSafe(ws, 1009, "URL too long");
        return;
      }
      try {
        parsedUrl = new URL(rawUrl, "http://localhost");
      } catch {
        wsCloseSafe(ws, 1002, "Invalid URL");
        return;
      }
    }
    const proto = parsedUrl.pathname === "/udp" ? "udp" : "tcp";

    const clientAddress = tryGetStringProp(tryGetProp(req, "socket"), "remoteAddress") ?? null;

    // `/udp` can operate in one of two modes:
    // 1) Per-target (legacy): `/udp?host=...&port=...` (or `target=...`) where WS messages are raw datagrams.
    // 2) Multiplexed (new): `/udp` with no target params, using v1/v2 framing (see proxy/webrtc-udp-relay/PROTOCOL.md).
    const hasHost = parsedUrl.searchParams.has("host");
    const hasPort = parsedUrl.searchParams.has("port");
    const hasTarget = parsedUrl.searchParams.has("target");
    if (proto === "udp" && !hasHost && !hasPort && !hasTarget) {
      log("info", "connect_requested", { connId, proto, mode: "multiplexed", clientAddress });
      log("info", "connect_accepted", { connId, proto, mode: "multiplexed", clientAddress });
      void handleUdpRelayMultiplexed(ws, connId, config, metrics).catch((err) => {
        log("error", "connect_error", { connId, proto, mode: "multiplexed", clientAddress, err: formatError(err) });
        metrics.incConnectionError("error");
        wsCloseSafe(ws, 1011, "Proxy error");
      });
      return;
    }

    const parsedTarget = parseTargetQuery(parsedUrl);
    if ("error" in parsedTarget) {
      log("warn", "connect_denied", {
        connId,
        proto,
        clientAddress,
        reason: parsedTarget.error
      });
      metrics.incConnectionError("denied");
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
          metrics.incConnectionError("denied");
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
          await handleUdpRelay(ws, connId, decision.target.resolvedAddress, decision.target.family, port, config, metrics);
        } else {
          await handleTcpRelay(ws, connId, decision.target.resolvedAddress, decision.target.family, port, config, metrics);
        }
      } catch (err) {
        log("error", "connect_error", { connId, proto, host, port, clientAddress, err: formatError(err) });
        metrics.incConnectionError("error");
        wsCloseSafe(ws, 1011, "Proxy error");
      }
    })();
  });

  wssMux.on("connection", (ws, req) => {
    const connId = nextConnId++;
    const clientAddress = tryGetStringProp(tryGetProp(req, "socket"), "remoteAddress") ?? null;
    handleTcpMuxRelay(ws, connId, clientAddress, config, metrics);
  });

  await new Promise<void>((resolve, reject) => {
    server.once("error", reject);
    server.listen(config.listenPort, config.listenHost, () => resolve());
  });

  const addr = server.address();
  const listenAddress =
    typeof addr === "string" ? addr : `http://${addr?.address ?? config.listenHost}:${addr?.port ?? config.listenPort}`;

  const allowConfigured = config.allow.trim().length > 0;
  const policyMode = config.open ? "open" : allowConfigured ? "allowlist" : "public_only";
  log("info", "proxy_start", { listenAddress, policyMode });

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

// UDP relay handlers are implemented in `udpRelay.ts`.
