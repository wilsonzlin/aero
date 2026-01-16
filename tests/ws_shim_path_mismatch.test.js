import test from "node:test";
import assert from "node:assert/strict";
import http from "node:http";
import net from "node:net";
import { randomBytes } from "node:crypto";

import { WebSocketServer } from "../scripts/ws-shim.mjs";

function once(emitter, event) {
  return new Promise((resolve) => emitter.once(event, resolve));
}

test("ws-shim: WebSocketServer({ path }) destroys socket on path mismatch", async () => {
  const server = http.createServer();
  const wss = new WebSocketServer({ server, path: "/ws" });

  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  const addr = server.address();
  assert.ok(addr && typeof addr === "object");
  const port = addr.port;

  const sock = net.connect({ host: "127.0.0.1", port });
  await once(sock, "connect");

  const key = randomBytes(16).toString("base64");
  const req =
    `GET /nope HTTP/1.1\r\n` +
    `Host: 127.0.0.1:${port}\r\n` +
    `Connection: Upgrade\r\n` +
    `Upgrade: websocket\r\n` +
    `Sec-WebSocket-Version: 13\r\n` +
    `Sec-WebSocket-Key: ${key}\r\n` +
    `\r\n`;

  sock.write(req);

  await Promise.race([
    once(sock, "close"),
    once(sock, "end"),
    new Promise((_, reject) => setTimeout(() => reject(new Error("socket not closed")), 2000)),
  ]);

  await new Promise((resolve) => wss.close(resolve));
  await new Promise((resolve) => server.close(resolve));
});

