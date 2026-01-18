import type { WebSocket } from "ws";

export function wsBufferedAmountSafe(ws: WebSocket): number {
  try {
    const value = (ws as unknown as { bufferedAmount?: unknown }).bufferedAmount;
    return Number.isFinite(value) && (value as number) >= 0 ? (value as number) : 0;
  } catch {
    return 0;
  }
}

