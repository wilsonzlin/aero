import { createHash } from "node:crypto";
import type { Duplex } from "node:stream";

import { isValidHttpToken } from "../httpTokens.js";

const WS_GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

function webSocketAccept(key: string): string {
  return createHash("sha1").update(key + WS_GUID).digest("base64");
}

export function writeWebSocketHandshake(
  socket: Duplex,
  opts: Readonly<{ key: string; protocol?: string }>,
): void {
  const accept = webSocketAccept(opts.key);
  const protocol = opts.protocol && isValidHttpToken(opts.protocol) ? opts.protocol : undefined;
  const response = [
    "HTTP/1.1 101 Switching Protocols",
    "Upgrade: websocket",
    "Connection: Upgrade",
    `Sec-WebSocket-Accept: ${accept}`,
    ...(protocol ? [`Sec-WebSocket-Protocol: ${protocol}`] : []),
    "\r\n",
  ].join("\r\n");
  try {
    socket.write(response);
  } catch {
    try {
      socket.destroy();
    } catch {
      // ignore
    }
  }
}

