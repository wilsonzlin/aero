import { createHash } from "node:crypto";

import { isValidHttpToken } from "./httpTokens.js";
import { destroyBestEffort, writeCaptureErrorBestEffort } from "./socket_safe.js";

const WS_GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

export function computeWebSocketAccept(key) {
  if (typeof key !== "string" || key.length === 0) {
    throw new TypeError("computeWebSocketAccept: key must be a non-empty string");
  }
  return createHash("sha1")
    .update(`${key}${WS_GUID}`, "utf8")
    .digest("base64");
}

export function encodeWebSocketHandshakeResponse(opts) {
  const key = opts?.key;
  const protocol = opts?.protocol;

  const accept = computeWebSocketAccept(key);
  const selectedProtocol =
    typeof protocol === "string" && protocol.length > 0 && isValidHttpToken(protocol) ? protocol : null;

  return [
    "HTTP/1.1 101 Switching Protocols",
    "Upgrade: websocket",
    "Connection: Upgrade",
    `Sec-WebSocket-Accept: ${accept}`,
    ...(selectedProtocol ? [`Sec-WebSocket-Protocol: ${selectedProtocol}`] : []),
    "",
    "",
  ].join("\r\n");
}

export function writeWebSocketHandshake(socket, opts) {
  const response = encodeWebSocketHandshakeResponse(opts);
  const res = writeCaptureErrorBestEffort(socket, response);
  if (res.err) destroyBestEffort(socket);
}

