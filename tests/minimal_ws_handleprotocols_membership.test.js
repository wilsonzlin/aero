import test from "node:test";
import assert from "node:assert/strict";
import net from "node:net";
import { randomBytes } from "node:crypto";

import { WebSocketServer } from "../tools/minimal_ws.js";

function once(emitter, event) {
  return new Promise((resolve) => emitter.once(event, resolve));
}

async function sendRawRequest(host, port, request) {
  return await new Promise((resolve, reject) => {
    const socket = net.connect({ host, port });
    const chunks = [];

    const cleanup = () => {
      socket.removeAllListeners();
      try {
        socket.destroy();
      } catch {
        // ignore
      }
    };

    socket.on("error", (err) => {
      cleanup();
      reject(err);
    });

    socket.on("data", (chunk) => {
      chunks.push(Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk));
      const text = Buffer.concat(chunks).toString("utf8");
      if (text.includes("\r\n\r\n")) {
        cleanup();
        resolve(text);
      }
    });

    socket.write(request);
  });
}

test("minimal_ws: handleProtocols must select an offered subprotocol", async () => {
  const wss = new WebSocketServer({
    host: "127.0.0.1",
    port: 0,
    handleProtocols: () => "not-offered",
  });
  await once(wss, "listening");
  const addr = wss.address();
  assert.ok(addr && typeof addr === "object");

  try {
    const key = randomBytes(16).toString("base64");
    const req =
      `GET / HTTP/1.1\r\n` +
      `Host: 127.0.0.1:${addr.port}\r\n` +
      `Connection: Upgrade\r\n` +
      `Upgrade: websocket\r\n` +
      `Sec-WebSocket-Version: 13\r\n` +
      `Sec-WebSocket-Key: ${key}\r\n` +
      `Sec-WebSocket-Protocol: offered\r\n` +
      `\r\n`;

    const res = await sendRawRequest("127.0.0.1", addr.port, req);
    assert.ok(res.startsWith("HTTP/1.1 400 "));
  } finally {
    await new Promise((resolve) => wss.close(resolve));
  }
});

test("minimal_ws server: rejects invalid Sec-WebSocket-Protocol tokens (400)", async () => {
  const wss = new WebSocketServer({ host: "127.0.0.1", port: 0 });
  await once(wss, "listening");
  const addr = wss.address();
  assert.ok(addr && typeof addr === "object");

  try {
    const key = randomBytes(16).toString("base64");
    const req =
      `GET / HTTP/1.1\r\n` +
      `Host: 127.0.0.1:${addr.port}\r\n` +
      `Connection: Upgrade\r\n` +
      `Upgrade: websocket\r\n` +
      `Sec-WebSocket-Version: 13\r\n` +
      `Sec-WebSocket-Key: ${key}\r\n` +
      `Sec-WebSocket-Protocol: a b\r\n` +
      `\r\n`;

    const res = await sendRawRequest("127.0.0.1", addr.port, req);
    assert.ok(res.startsWith("HTTP/1.1 400 "));
  } finally {
    await new Promise((resolve) => wss.close(resolve));
  }
});

