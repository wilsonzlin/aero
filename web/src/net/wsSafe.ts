import { formatOneLineUtf8 } from "../text.ts";

export type WebSocketSendData = string | ArrayBuffer | ArrayBufferView | Blob;

// RFC 6455 close reason is limited to 123 bytes (125 total payload bytes incl. 2-byte code).
const MAX_WS_CLOSE_REASON_BYTES = 123;

// WebSocket readyState constants per spec:
// - CONNECTING = 0
// - OPEN = 1
// - CLOSING = 2
// - CLOSED = 3
const WS_OPEN = 1;
const WS_CLOSED = 3;

export function wsIsOpenSafe(ws: WebSocket | null | undefined): boolean {
  if (!ws) return false;
  try {
    return ws.readyState === WS_OPEN;
  } catch {
    return false;
  }
}

export function wsIsClosedSafe(ws: WebSocket | null | undefined): boolean {
  if (!ws) return false;
  try {
    return ws.readyState === WS_CLOSED;
  } catch {
    return false;
  }
}

export function wsProtocolSafe(ws: WebSocket | null | undefined): string | null {
  if (!ws) return null;
  try {
    const proto = (ws as unknown as { protocol?: unknown }).protocol;
    return typeof proto === "string" ? proto : null;
  } catch {
    return null;
  }
}

export function wsSendSafe(ws: WebSocket, data: WebSocketSendData): boolean {
  if (!wsIsOpenSafe(ws)) return false;
  try {
    ws.send(data);
    return true;
  } catch {
    return false;
  }
}

export function wsBufferedAmountSafe(ws: WebSocket | null | undefined): number {
  if (!ws) return 0;
  try {
    const value = (ws as unknown as { bufferedAmount?: unknown }).bufferedAmount;
    return Number.isFinite(value) && (value as number) >= 0 ? (value as number) : 0;
  } catch {
    return 0;
  }
}

export function wsCloseSafe(ws: WebSocket, code?: number, reason?: unknown): void {
  if (!ws || typeof ws.close !== "function") return;
  try {
    if (typeof code !== "number") {
      ws.close();
      return;
    }
    if (reason === undefined) {
      ws.close(code);
      return;
    }
    const safeReason = formatOneLineUtf8(reason, MAX_WS_CLOSE_REASON_BYTES);
    if (!safeReason) {
      ws.close(code);
      return;
    }
    ws.close(code, safeReason);
  } catch {
    // ignore
  }
}

