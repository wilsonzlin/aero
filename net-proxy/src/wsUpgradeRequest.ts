import type http from "node:http";

import { rawHeaderSingle } from "./rawHeaders";

export type WebSocketHandshakeDecision = { ok: true } | { ok: false; status: 400; message: string };

// Conservative caps for websocket handshake header values. These are attacker-controlled and should
// be bounded before we hand the socket to the `ws` library.
const MAX_UPGRADE_HEADER_LEN = 256;
const MAX_CONNECTION_HEADER_LEN = 256;
const MAX_SEC_WEBSOCKET_VERSION_LEN = 32;
const MAX_SEC_WEBSOCKET_KEY_LEN = 256;

function headerSingle(headers: http.IncomingHttpHeaders, name: string): string | undefined {
  const v = headers[name];
  if (typeof v === "string") return v;
  // Be strict: repeated handshake headers are ambiguous across stacks.
  if (Array.isArray(v)) {
    if (v.length === 0) return undefined;
    if (v.length === 1) return typeof v[0] === "string" ? v[0] : undefined;
    return undefined;
  }
  return undefined;
}

function headerHasToken(raw: string, needleLower: string): boolean {
  // Header lists use comma-separated tokens (RFC 7230). Avoid allocation-heavy `split()` on
  // attacker-controlled strings: scan tokens in-place.
  let start = 0;
  while (start < raw.length) {
    let end = raw.indexOf(",", start);
    if (end === -1) end = raw.length;

    // Trim ASCII whitespace.
    while (start < end && raw.charCodeAt(start) <= 0x20) start += 1;
    while (end > start && raw.charCodeAt(end - 1) <= 0x20) end -= 1;

    const len = end - start;
    if (len === needleLower.length) {
      let ok = true;
      for (let i = 0; i < len; i += 1) {
        let c = raw.charCodeAt(start + i);
        if (c >= 0x41 && c <= 0x5a) c += 0x20; // ASCII upper -> lower
        if (c !== needleLower.charCodeAt(i)) {
          ok = false;
          break;
        }
      }
      if (ok) return true;
    }

    start = end + 1;
  }
  return false;
}

export function validateWebSocketHandshakeRequest(req: http.IncomingMessage): WebSocketHandshakeDecision {
  const rawHeaders = (req as unknown as { rawHeaders?: unknown }).rawHeaders;

  const upgradeRaw = rawHeaderSingle(rawHeaders, "upgrade", MAX_UPGRADE_HEADER_LEN);
  if (upgradeRaw === null) {
    return { ok: false, status: 400, message: "Invalid WebSocket upgrade" };
  }
  const upgrade = upgradeRaw ?? headerSingle(req.headers, "upgrade");
  if (upgrade && upgrade.length > MAX_UPGRADE_HEADER_LEN) {
    return { ok: false, status: 400, message: "Invalid WebSocket upgrade" };
  }
  if (!upgrade || !headerHasToken(upgrade, "websocket")) {
    return { ok: false, status: 400, message: "Invalid WebSocket upgrade" };
  }

  const connectionRaw = rawHeaderSingle(rawHeaders, "connection", MAX_CONNECTION_HEADER_LEN);
  if (connectionRaw === null) {
    return { ok: false, status: 400, message: "Invalid WebSocket upgrade" };
  }
  const connection = connectionRaw ?? headerSingle(req.headers, "connection");
  if (connection && connection.length > MAX_CONNECTION_HEADER_LEN) {
    return { ok: false, status: 400, message: "Invalid WebSocket upgrade" };
  }
  if (!connection || !headerHasToken(connection, "upgrade")) {
    return { ok: false, status: 400, message: "Invalid WebSocket upgrade" };
  }

  const versionRaw = rawHeaderSingle(rawHeaders, "sec-websocket-version", MAX_SEC_WEBSOCKET_VERSION_LEN);
  if (versionRaw === null) {
    return { ok: false, status: 400, message: "Invalid WebSocket upgrade" };
  }
  const version = versionRaw ?? headerSingle(req.headers, "sec-websocket-version");
  if (version && version.length > MAX_SEC_WEBSOCKET_VERSION_LEN) {
    return { ok: false, status: 400, message: "Invalid WebSocket upgrade" };
  }
  if (!version || version.trim() !== "13") {
    return { ok: false, status: 400, message: "Invalid WebSocket upgrade" };
  }

  const keyRaw = rawHeaderSingle(rawHeaders, "sec-websocket-key", MAX_SEC_WEBSOCKET_KEY_LEN);
  if (keyRaw === null) {
    return { ok: false, status: 400, message: "Invalid WebSocket upgrade" };
  }
  const key = keyRaw ?? headerSingle(req.headers, "sec-websocket-key");
  if (key && key.length > MAX_SEC_WEBSOCKET_KEY_LEN) {
    return { ok: false, status: 400, message: "Invalid WebSocket upgrade" };
  }
  if (!key || key.trim().length === 0) {
    return { ok: false, status: 400, message: "Missing required header: Sec-WebSocket-Key" };
  }

  return { ok: true };
}

