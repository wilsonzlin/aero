import { formatOneLineUtf8 } from "../text.ts";

export type WebSocketSendData = string | ArrayBuffer | ArrayBufferView | Blob;

// RFC 6455 close reason is limited to 123 bytes (125 total payload bytes incl. 2-byte code).
const MAX_WS_CLOSE_REASON_BYTES = 123;

export function wsSendSafe(ws: WebSocket, data: WebSocketSendData): boolean {
  try {
    if (ws.readyState !== WebSocket.OPEN) return false;
  } catch {
    return false;
  }
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
    return Number.isFinite(value) ? (value as number) : 0;
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

