import http from "node:http";
import type { Duplex } from "node:stream";

import { WebSocketServer, type WebSocket } from "ws";

import { loadConfigFromEnv, type L2ProxyConfig } from "./config.js";
import { ConnectionCounter, SessionQuota } from "./limits.js";
import { chooseL2Subprotocol, L2_TUNNEL_PATH, validateL2WsUpgrade } from "./policy.js";

export interface RunningL2ProxyServer {
  server: http.Server;
  config: L2ProxyConfig;
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
  ws.close(code, truncateCloseReason(reason));
}

function respondHttp(socket: Duplex, status: number, message: string): void {
  const body = `${message}\n`;
  // NOTE: Be careful to emit exactly one empty line between headers and body.
  // Adding extra `\r\n` bytes would make `Content-Length` incorrect and can
  // cause WebSocket clients to report parse errors for `unexpected-response`.
  socket.end(
    [
      `HTTP/1.1 ${status} ${httpStatusText(status)}`,
      "Content-Type: text/plain; charset=utf-8",
      `Content-Length: ${Buffer.byteLength(body)}`,
      "Connection: close",
      "",
      body,
    ].join("\r\n"),
  );
}

function httpStatusText(status: number): string {
  switch (status) {
    case 200:
      return "OK";
    case 400:
      return "Bad Request";
    case 401:
      return "Unauthorized";
    case 403:
      return "Forbidden";
    case 404:
      return "Not Found";
    case 429:
      return "Too Many Requests";
    default:
      return "Error";
  }
}

function byteLength(data: unknown): number {
  if (typeof data === "string") return Buffer.byteLength(data);
  if (data instanceof ArrayBuffer) return data.byteLength;
  if (ArrayBuffer.isView(data)) return data.byteLength;
  if (Buffer.isBuffer(data)) return data.byteLength;
  return 0;
}

export async function startL2ProxyServer(overrides: Partial<L2ProxyConfig> = {}): Promise<RunningL2ProxyServer> {
  const config: L2ProxyConfig = { ...loadConfigFromEnv(), ...overrides };

  const server = http.createServer((req, res) => {
    const url = new URL(req.url ?? "/", "http://localhost");
    if (req.method === "GET" && url.pathname === "/healthz") {
      res.writeHead(200, { "content-type": "application/json; charset=utf-8" });
      res.end(JSON.stringify({ ok: true }));
      return;
    }

    res.writeHead(404, { "content-type": "text/plain; charset=utf-8" });
    res.end("not found\n");
  });

  const connCounter = new ConnectionCounter(config.maxConnections);

  const wss = new WebSocketServer({
    noServer: true,
    maxPayload: config.wsMaxPayloadBytes,
    handleProtocols: (protocols, req) => {
      const offeredHeader = req.headers["sec-websocket-protocol"];
      const offered = (Array.isArray(offeredHeader) ? offeredHeader.join(",") : offeredHeader ?? "")
        .split(",")
        .map((p) => p.trim())
        .filter((p) => p.length > 0);
      const selected = chooseL2Subprotocol(offered);
      if (!selected) return false;
      return protocols.has(selected) ? selected : false;
    },
  });

  server.on("upgrade", (req, socket, head) => {
    let url: URL;
    try {
      url = new URL(req.url ?? "/", "http://localhost");
    } catch {
      respondHttp(socket, 400, "Invalid request");
      return;
    }

    if (url.pathname !== L2_TUNNEL_PATH && url.pathname !== "/") {
      respondHttp(socket, 404, "Not Found");
      return;
    }

    const decision = validateL2WsUpgrade(req, url, config, connCounter.getActive());
    if (!decision.ok) {
      respondHttp(socket, decision.status, decision.message);
      return;
    }

    if (!connCounter.acquire()) {
      respondHttp(socket, 429, "Too many connections");
      return;
    }

    const releaseOnce = (() => {
      let released = false;
      return () => {
        if (released) return;
        released = true;
        connCounter.release();
      };
    })();

    try {
      wss.handleUpgrade(req, socket, head, (ws) => {
        const quota = new SessionQuota({
          maxBytes: config.maxBytesPerConnection,
          maxFramesPerSecond: config.maxFramesPerSecond,
        });

        ws.on("message", (data) => {
          const len = byteLength(data);
          const q = quota.onRxFrame(len);
          if (!q.ok) {
            wsCloseSafe(ws, 1008, q.reason);
          }
        });

        const originalSend = ws.send.bind(ws);
        ws.send = ((data: unknown, ...args: unknown[]) => {
          const len = byteLength(data);
          const q = quota.onTxFrame(len);
          if (!q.ok) {
            wsCloseSafe(ws, 1008, q.reason);
            return;
          }
          // @ts-expect-error - ws has multiple overloads; we forward dynamically.
          return originalSend(data, ...args);
        }) as typeof ws.send;

        ws.on("close", releaseOnce);
        ws.on("error", releaseOnce);
      });
    } catch {
      releaseOnce();
      respondHttp(socket, 500, "WebSocket upgrade failed");
    }
  });

  await new Promise<void>((resolve, reject) => {
    server.once("error", reject);
    server.listen(config.listenPort, config.listenHost, () => resolve());
  });

  const addr = server.address();
  const listenAddress =
    typeof addr === "string" ? addr : `http://${addr?.address ?? config.listenHost}:${addr?.port ?? config.listenPort}`;

  return {
    server,
    config,
    listenAddress,
    close: async () => {
      for (const client of wss.clients) {
        client.terminate();
      }
      await new Promise<void>((resolve) => wss.close(() => resolve()));
      await new Promise<void>((resolve, reject) => server.close((err) => (err ? reject(err) : resolve())));
    },
  };
}
