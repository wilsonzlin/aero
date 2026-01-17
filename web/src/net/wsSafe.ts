export type WebSocketSendData = string | ArrayBuffer | ArrayBufferView;

export function wsSendSafe(ws: WebSocket, data: WebSocketSendData): boolean {
  if (ws.readyState !== WebSocket.OPEN) return false;
  try {
    ws.send(data);
    return true;
  } catch {
    return false;
  }
}

export function wsCloseSafe(ws: WebSocket, code?: number, reason?: string): void {
  try {
    if (code === undefined) ws.close();
    else ws.close(code, reason);
  } catch {
    // ignore
  }
}

