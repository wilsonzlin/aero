import net from "node:net";
import type http from "node:http";
import type { Duplex } from "node:stream";

import type { TcpTarget } from "../protocol/tcpTarget.js";
import { TcpTargetParseError, parseTcpTargetFromUrl } from "../protocol/tcpTarget.js";
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

type TcpProxyUpgradeOptions = TcpProxyUpgradePolicy &
  Readonly<{
    allowPrivateIps?: boolean;
    createConnection?: typeof net.createConnection;
    metrics?: TcpProxyEgressMetricSink;
    sessionId?: string;
    sessionConnections?: SessionConnectionTracker;
    maxMessageBytes?: number;
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
  const rawUrl = req.url ?? "";
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

    if ("setNoDelay" in socket && typeof socket.setNoDelay === "function") {
      socket.setNoDelay(true);
    }

    const connectTimeoutMs = opts.connectTimeoutMs ?? 10_000;
    const idleTimeoutMs = opts.idleTimeoutMs ?? 300_000;
    const createConnection = opts.createConnection ?? net.createConnection;
    const tcpSocket = createConnection({ host: resolved.ip, port: resolved.port });
    tcpSocket.setNoDelay(true);
    tcpSocket.setTimeout(idleTimeoutMs);
    tcpSocket.on("timeout", () => tcpSocket.destroy(new Error("TCP idle timeout")));

    const connectTimer = setTimeout(() => {
      tcpSocket.destroy(new Error("TCP connect timeout"));
    }, connectTimeoutMs);
    connectTimer.unref?.();
    tcpSocket.once("connect", () => clearTimeout(connectTimer));
    tcpSocket.once("error", () => clearTimeout(connectTimer));
    tcpSocket.once("close", () => clearTimeout(connectTimer));

    const bridge = new WebSocketTcpBridge(socket, tcpSocket, opts.maxMessageBytes ?? 1024 * 1024);
    bridge.start(head);
  })();
}

function formatUpgradeError(err: unknown, fallback: string): string {
  if (err instanceof TcpTargetParseError) {
    return formatOneLineError(err, MAX_UPGRADE_ERROR_MESSAGE_BYTES, fallback);
  }
  return fallback;
}
