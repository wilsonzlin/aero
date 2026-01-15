import { createHash } from "node:crypto";
import type { Duplex } from "node:stream";

const WS_GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

function webSocketAccept(key: string): string {
  return createHash("sha1").update(key + WS_GUID).digest("base64");
}

export function writeWebSocketHandshake(
  socket: Duplex,
  opts: Readonly<{ key: string; protocol?: string }>,
): void {
  const accept = webSocketAccept(opts.key);
  socket.write(
    [
      "HTTP/1.1 101 Switching Protocols",
      "Upgrade: websocket",
      "Connection: Upgrade",
      `Sec-WebSocket-Accept: ${accept}`,
      ...(opts.protocol ? [`Sec-WebSocket-Protocol: ${opts.protocol}`] : []),
      "\r\n",
    ].join("\r\n"),
  );
}

