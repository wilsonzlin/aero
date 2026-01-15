import type http from "node:http";

import { rawHeaderSingle } from "../rawHeaders.js";

export type WebSocketHandshakeDecision =
  | { ok: true; key: string }
  | { ok: false; status: 400; message: string };

// Defensive caps: these headers are attacker-controlled.
const MAX_UPGRADE_HEADER_LEN = 256;
const MAX_CONNECTION_HEADER_LEN = 256;
const MAX_WS_VERSION_HEADER_LEN = 32;
const MAX_WS_KEY_HEADER_LEN = 256;

export function sanitizeWebSocketHandshakeKey(key: unknown): string | undefined {
  if (typeof key !== "string") return undefined;
  const trimmed = key.trim();
  if (trimmed === "") return undefined;
  if (trimmed.length > MAX_WS_KEY_HEADER_LEN) return undefined;
  return trimmed;
}

function headerSingle(headers: Record<string, unknown>, name: string): string | undefined {
  const v = headers[name];
  if (typeof v === "string") return v;
  // Be strict with repeated headers: different stacks disagree on join order and
  // delimiters, and the WebSocket handshake requires unambiguous single values.
  if (Array.isArray(v)) {
    if (v.length === 0) return undefined;
    if (v.length === 1) return typeof v[0] === "string" ? v[0] : undefined;
    return undefined;
  }
  return undefined;
}

function headerHasToken(raw: string, needleLower: string): boolean {
  // Header lists use comma-separated tokens (RFC 7230).
  // Be strict but allocation-light: scan tokens without building a full array.
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
  const headers = (req as unknown as { headers?: Record<string, unknown> }).headers ?? {};
  const rawHeaders = (req as unknown as { rawHeaders?: unknown }).rawHeaders;

  const upgradeRaw = rawHeaderSingle(rawHeaders, "upgrade", MAX_UPGRADE_HEADER_LEN);
  if (upgradeRaw === null) {
    return { ok: false, status: 400, message: "Invalid WebSocket upgrade" };
  }
  const upgrade = upgradeRaw ?? headerSingle(headers, "upgrade");
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
  const connection = connectionRaw ?? headerSingle(headers, "connection");
  if (connection && connection.length > MAX_CONNECTION_HEADER_LEN) {
    return { ok: false, status: 400, message: "Invalid WebSocket upgrade" };
  }
  if (!connection || !headerHasToken(connection, "upgrade")) {
    return { ok: false, status: 400, message: "Invalid WebSocket upgrade" };
  }

  const versionRaw = rawHeaderSingle(rawHeaders, "sec-websocket-version", MAX_WS_VERSION_HEADER_LEN);
  if (versionRaw === null) {
    return { ok: false, status: 400, message: "Invalid WebSocket upgrade" };
  }
  const version = versionRaw ?? headerSingle(headers, "sec-websocket-version");
  if (version && version.length > MAX_WS_VERSION_HEADER_LEN) {
    return { ok: false, status: 400, message: "Invalid WebSocket upgrade" };
  }
  if (!version || version.trim() !== "13") {
    return { ok: false, status: 400, message: "Invalid WebSocket upgrade" };
  }

  const keyRaw = rawHeaderSingle(rawHeaders, "sec-websocket-key", MAX_WS_KEY_HEADER_LEN);
  if (keyRaw === null) {
    return { ok: false, status: 400, message: "Invalid WebSocket upgrade" };
  }
  const key = keyRaw ?? headerSingle(headers, "sec-websocket-key");
  if (key && key.length > MAX_WS_KEY_HEADER_LEN) {
    return { ok: false, status: 400, message: "Invalid WebSocket upgrade" };
  }
  const keyTrimmed = sanitizeWebSocketHandshakeKey(key) ?? "";
  if (keyTrimmed === "") {
    return { ok: false, status: 400, message: "Missing required header: Sec-WebSocket-Key" };
  }

  return { ok: true, key: keyTrimmed };
}

