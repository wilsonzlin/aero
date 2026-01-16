import test from "node:test";
import assert from "node:assert/strict";
import http from "node:http";
import net from "node:net";
import { randomBytes } from "node:crypto";

import { WebSocketServer } from "../scripts/ws-shim.mjs";

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

function handshake({ port, extraHeaders = "" }) {
  const key = randomBytes(16).toString("base64");
  return (
    `GET / HTTP/1.1\r\n` +
    `Host: 127.0.0.1:${port}\r\n` +
    `Connection: Upgrade\r\n` +
    `Upgrade: websocket\r\n` +
    `Sec-WebSocket-Version: 13\r\n` +
    `Sec-WebSocket-Key: ${key}\r\n` +
    extraHeaders +
    `\r\n`
  );
}

test("ws-shim server: rejects oversized Sec-WebSocket-Protocol header (400)", async () => {
  const server = http.createServer();
  const wss = new WebSocketServer({ server });

  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  const addr = server.address();
  assert.ok(addr && typeof addr === "object");

  try {
    const req = handshake({
      port: addr.port,
      extraHeaders: `Sec-WebSocket-Protocol: ${"a".repeat(5000)}\r\n`,
    });
    const res = await sendRawRequest("127.0.0.1", addr.port, req);
    assert.ok(res.startsWith("HTTP/1.1 400 "));
  } finally {
    await new Promise((resolve) => wss.close(resolve));
    await new Promise((resolve) => server.close(resolve));
  }
});

test("ws-shim server: rejects too many Sec-WebSocket-Protocol tokens (400)", async () => {
  const server = http.createServer();
  const wss = new WebSocketServer({ server });

  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  const addr = server.address();
  assert.ok(addr && typeof addr === "object");

  try {
    const tokens = Array.from({ length: 33 }, (_v, i) => `p${i}`).join(", ");
    const req = handshake({
      port: addr.port,
      extraHeaders: `Sec-WebSocket-Protocol: ${tokens}\r\n`,
    });
    const res = await sendRawRequest("127.0.0.1", addr.port, req);
    assert.ok(res.startsWith("HTTP/1.1 400 "));
  } finally {
    await new Promise((resolve) => wss.close(resolve));
    await new Promise((resolve) => server.close(resolve));
  }
});

test("ws-shim server: rejects invalid Sec-WebSocket-Protocol tokens (400)", async () => {
  const server = http.createServer();
  const wss = new WebSocketServer({ server });

  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  const addr = server.address();
  assert.ok(addr && typeof addr === "object");

  try {
    const req = handshake({
      port: addr.port,
      extraHeaders: `Sec-WebSocket-Protocol: a b\r\n`,
    });
    const res = await sendRawRequest("127.0.0.1", addr.port, req);
    assert.ok(res.startsWith("HTTP/1.1 400 "));
  } finally {
    await new Promise((resolve) => wss.close(resolve));
    await new Promise((resolve) => server.close(resolve));
  }
});

