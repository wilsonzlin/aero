import type { WebSocket } from "ws";

function truncateCloseReason(reason: string, maxBytes = 123): string {
  const buf = Buffer.from(reason, "utf8");
  if (buf.length <= maxBytes) return reason;

  let truncated = buf.subarray(0, maxBytes).toString("utf8");
  while (Buffer.byteLength(truncated, "utf8") > maxBytes) {
    truncated = truncated.slice(0, -1);
  }
  return truncated;
}

export function wsCloseSafe(ws: WebSocket, code: number, reason: string): void {
  const safeReason = truncateCloseReason(reason);
  ws.close(code, safeReason);
}

