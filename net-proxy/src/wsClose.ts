import type { WebSocket } from "ws";

import { formatOneLineUtf8 } from "./text";

export function wsCloseSafe(ws: WebSocket, code: number, reason: string): void {
  // RFC6455 close reason is limited to 123 bytes.
  ws.close(code, formatOneLineUtf8(reason, 123));
}

