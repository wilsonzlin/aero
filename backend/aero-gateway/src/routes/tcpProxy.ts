import net from "node:net";
import type http from "node:http";
import type { Duplex } from "node:stream";

import type { TcpTarget } from "../protocol/tcpTarget.js";
import { TcpTargetParseError, parseTcpTargetFromUrl } from "../protocol/tcpTarget.js";
import { tryGetStringProp } from "../../../../src/safe_props.js";
import { validateTcpTargetPolicy, validateWsUpgradePolicy, type TcpProxyUpgradePolicy } from "./tcpPolicy.js";
import { enforceUpgradeRequestUrlLimit, resolveUpgradeRequestUrl, respondUpgradeHttp } from "./upgradeHttp.js";
import { writeWebSocketHandshake } from "./wsHandshake.js";
import { sanitizeWebSocketHandshakeKey, validateWebSocketHandshakeRequest } from "./wsUpgradeRequest.js";
import type { SessionConnectionTracker } from "../session.js";
import { WebSocketTcpBridge } from "./tcpBridge.js";
import type { TcpProxyEgressMetricSink } from "./tcpEgressMetrics.js";
import { resolveTcpProxyTarget, TcpProxyTargetError } from "./tcpResolve.js";
import { formatOneLineError } from "../util/text.js";

const MAX_UPGRADE_ERROR_MESSAGE_BYTES = 512;

function isSocketDestroyed(socket: Duplex): boolean {
  try {
    return (socket as unknown as { destroyed?: unknown }).destroyed === true;
  } catch {
    // Fail closed: if state is not observable, treat it as destroyed.
    return true;
  }
}

type TcpProxyUpgradeOptions = TcpProxyUpgradePolicy &
  Readonly<{
    allowPrivateIps?: boolean;
    createConnection?: typeof net.createConnection;
    metrics?: TcpProxyEgressMetricSink;
    sessionId?: string;
    sessionConnections?: SessionConnectionTracker;
    maxMessageBytes?: number;
    maxTcpBufferedBytes?: number;
    connectTimeoutMs?: number;
    idleTimeoutMs?: number;
    /**
     * If provided, the caller has already validated the RFC6455 handshake and extracted a trimmed
     * `Sec-WebSocket-Key`. This avoids re-validating the same handshake in router code that already
     * does upgrade gating.
     */
    handshakeKey?: string;
    /**
     * If provided, the caller has already parsed `req.url`.
     *
     * This avoids repeating `new URL(...)` in router code that already needed the parsed URL for
     * upgrade dispatch.
     */
    upgradeUrl?: URL;
  }>;

export function handleTcpProxyUpgrade(
  req: http.IncomingMessage,
  socket: Duplex,
  head: Buffer,
  opts: TcpProxyUpgradeOptions = {},
): void {
  const rawUrl = tryGetStringProp(req, "url") ?? "";
  if (!enforceUpgradeRequestUrlLimit(rawUrl, socket, opts.upgradeUrl)) return;

  let handshakeKey = sanitizeWebSocketHandshakeKey(opts.handshakeKey);
  if (!handshakeKey) {
    const handshake = validateWebSocketHandshakeRequest(req);
    if (!handshake.ok) {
      respondUpgradeHttp(socket, handshake.status, handshake.message);
      return;
    }
    handshakeKey = handshake.key;
  }

  const upgradeDecision = validateWsUpgradePolicy(req, opts);
  if (!upgradeDecision.ok) {
    respondUpgradeHttp(socket, upgradeDecision.status, upgradeDecision.message);
    return;
  }

  let target: TcpTarget;
  try {
    const url = resolveUpgradeRequestUrl(rawUrl, socket, opts.upgradeUrl, "Invalid request");
    if (!url) return;
    if (!opts.upgradeUrl && url.pathname !== "/tcp") {
      respondUpgradeHttp(socket, 404, "Not Found");
      return;
    }
    target = parseTcpTargetFromUrl(url);
  } catch (err) {
    respondUpgradeHttp(socket, 400, formatUpgradeError(err, "Invalid request"));
    return;
  }

  const targetDecision = validateTcpTargetPolicy(target.host, target.port, opts);
  if (!targetDecision.ok) {
    respondUpgradeHttp(socket, targetDecision.status, targetDecision.message);
    return;
  }

  void (async () => {
    try {
      let resolved: { ip: string; port: number; hostname?: string };
      try {
        resolved = await resolveTcpProxyTarget(target.host, target.port, {
          allowPrivateIps: opts.allowPrivateIps,
          metrics: opts.metrics,
        });
      } catch (err) {
        if (err instanceof TcpProxyTargetError) {
          respondUpgradeHttp(socket, err.statusCode, formatOneLineError(err, MAX_UPGRADE_ERROR_MESSAGE_BYTES));
          return;
        }
        respondUpgradeHttp(socket, 502, formatUpgradeError(err, "Bad Gateway"));
        return;
      }

      if (opts.sessionId && opts.sessionConnections) {
        if (!opts.sessionConnections.tryAcquire(opts.sessionId)) {
          respondUpgradeHttp(socket, 429, "Too many concurrent connections");
          return;
        }

        let released = false;
        const release = () => {
          if (released) return;
          released = true;
          opts.sessionConnections!.release(opts.sessionId!);
        };
        socket.once("close", release);
        socket.once("error", release);
      }

      writeWebSocketHandshake(socket, { key: handshakeKey });
      // `writeWebSocketHandshake` destroys the socket if `write(...)` throws. Avoid continuing the
      // upgrade flow if the socket is already torn down.
      if (isSocketDestroyed(socket)) return;

      if ("setNoDelay" in socket && typeof socket.setNoDelay === "function") {
        socket.setNoDelay(true);
      }

      const connectTimeoutMs = opts.connectTimeoutMs ?? 10_000;
      const idleTimeoutMs = opts.idleTimeoutMs ?? 300_000;
      const createConnection = opts.createConnection ?? net.createConnection;
      let tcpSocket: net.Socket;
      try {
        tcpSocket = createConnection({ host: resolved.ip, port: resolved.port });
      } catch {
        try {
          socket.destroy();
        } catch {
          // ignore
        }
        return;
      }
      tcpSocket.setNoDelay(true);
      tcpSocket.setTimeout(idleTimeoutMs);
      tcpSocket.on("timeout", () => {
        try {
          tcpSocket.destroy(new Error("TCP idle timeout"));
        } catch {
          // ignore
        }
      });

      const connectTimer = setTimeout(() => {
        try {
          tcpSocket.destroy(new Error("TCP connect timeout"));
        } catch {
          // ignore
        }
      }, connectTimeoutMs);
      connectTimer.unref?.();
      tcpSocket.once("connect", () => clearTimeout(connectTimer));
      tcpSocket.once("error", () => clearTimeout(connectTimer));
      tcpSocket.once("close", () => clearTimeout(connectTimer));

      const bridge = new WebSocketTcpBridge(socket, tcpSocket, {
        maxMessageBytes: opts.maxMessageBytes ?? 1024 * 1024,
        maxTcpBufferedBytes: opts.maxTcpBufferedBytes ?? 10 * 1024 * 1024,
      });
      bridge.start(head);
    } catch {
      // Defensive: avoid unhandled rejections crashing the gateway on unexpected errors.
      try {
        socket.destroy();
      } catch {
        // ignore
      }
    }
  })();
}

function formatUpgradeError(err: unknown, fallback: string): string {
  if (err instanceof TcpTargetParseError) {
    return formatOneLineError(err, MAX_UPGRADE_ERROR_MESSAGE_BYTES, fallback);
  }
  return fallback;
}
