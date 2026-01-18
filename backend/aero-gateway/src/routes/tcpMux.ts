import type http from "node:http";
import type { Duplex } from "node:stream";

import {
  TCP_MUX_SUBPROTOCOL,
} from "../protocol/tcpMux.js";
import { tryGetProp, tryGetStringProp } from "./safeProps.js";
import { validateWsUpgradePolicy } from "./tcpPolicy.js";
import { enforceUpgradeRequestUrlLimit, resolveUpgradeRequestUrl, respondUpgradeHttp } from "./upgradeHttp.js";
import { writeWebSocketHandshake } from "./wsHandshake.js";
import { sanitizeWebSocketHandshakeKey, validateWebSocketHandshakeRequest } from "./wsUpgradeRequest.js";
import { hasWebSocketSubprotocol } from "./wsSubprotocol.js";
import { WebSocketTcpMuxBridge, type TcpMuxBridgeOptions } from "./tcpMuxBridge.js";
import { setNoDelayBestEffort } from "./socketSafe.js";

function isSocketDestroyed(socket: Duplex): boolean {
  try {
    return (socket as unknown as { destroyed?: unknown }).destroyed === true;
  } catch {
    // Fail closed: if state is not observable, treat it as destroyed.
    return true;
  }
}

type TcpMuxUpgradeOptions = TcpMuxBridgeOptions &
  Readonly<{
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

export function handleTcpMuxUpgrade(
  req: http.IncomingMessage,
  socket: Duplex,
  head: Buffer,
  opts: TcpMuxUpgradeOptions = {},
): void {
  const rawUrl = opts.upgradeUrl ? "" : tryGetStringProp(req, "url");
  if (!opts.upgradeUrl && (!rawUrl || rawUrl === "" || rawUrl.trim() !== rawUrl)) {
    respondUpgradeHttp(socket, 400, "Invalid request");
    return;
  }
  if (!enforceUpgradeRequestUrlLimit(rawUrl ?? "", socket, opts.upgradeUrl)) return;

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

  const url = resolveUpgradeRequestUrl(rawUrl ?? "", socket, opts.upgradeUrl, "Invalid request");
  if (!url) return;
  if (!opts.upgradeUrl && url.pathname !== "/tcp-mux") {
    respondUpgradeHttp(socket, 404, "Not Found");
    return;
  }

  const protocolHeaderRaw = tryGetProp(tryGetProp(req, "headers"), "sec-websocket-protocol");
  const protocolHeader =
    typeof protocolHeaderRaw === "string" || Array.isArray(protocolHeaderRaw) ? protocolHeaderRaw : undefined;
  const subprotocol = hasWebSocketSubprotocol(protocolHeader, TCP_MUX_SUBPROTOCOL);
  if (!subprotocol.ok) {
    respondUpgradeHttp(socket, 400, "Invalid Sec-WebSocket-Protocol header");
    return;
  }
  if (!subprotocol.has) {
    respondUpgradeHttp(socket, 400, `Missing required subprotocol: ${TCP_MUX_SUBPROTOCOL}`);
    return;
  }

  writeWebSocketHandshake(socket, { key: handshakeKey, protocol: TCP_MUX_SUBPROTOCOL });

  // `writeWebSocketHandshake` destroys the socket if `write(...)` throws. Avoid continuing the
  // upgrade flow if the socket is already torn down.
  if (isSocketDestroyed(socket)) return;

  setNoDelayBestEffort(socket, true);

  const bridge = new WebSocketTcpMuxBridge(socket, opts);
  bridge.start(head);
}
