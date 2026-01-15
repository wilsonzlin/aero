import test from "node:test";
import assert from "node:assert/strict";
import net from "node:net";
import { startProxyServer } from "../server";

async function sendRawUpgradeRequest(
  host: string,
  port: number,
  request: string
): Promise<{ status: number; body: string }> {
  return await new Promise((resolve, reject) => {
    const socket = net.connect({ host, port });
    const chunks: Buffer[] = [];

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
    });

    socket.on("end", () => {
      const buf = Buffer.concat(chunks);
      const text = buf.toString("utf8");
      const headEnd = text.indexOf("\r\n\r\n");
      const head = headEnd === -1 ? text : text.slice(0, headEnd);
      const body = headEnd === -1 ? "" : text.slice(headEnd + 4);
      const statusLine = head.split("\r\n", 1)[0] ?? "";
      const m = /^HTTP\/1\.[01]\s+(\d{3})\b/.exec(statusLine);
      const status = m ? Number(m[1]) : 0;
      cleanup();
      resolve({ status, body });
    });

    socket.write(request);
    socket.end();
  });
}

function makeKey(): string {
  return Buffer.from("dGhlIHNhbXBsZSBub25jZQ==", "base64").toString("base64");
}

test("websocket upgrade rejects missing Sec-WebSocket-Key (400)", async () => {
  const proxy = await startProxyServer({ listenHost: "127.0.0.1", listenPort: 0, open: true });
  const addr = proxy.server.address();
  assert.ok(addr && typeof addr !== "string");

  try {
    const res = await sendRawUpgradeRequest(
      "127.0.0.1",
      addr.port,
      [
        "GET /tcp?host=127.0.0.1&port=1 HTTP/1.1",
        `Host: 127.0.0.1:${addr.port}`,
        "Upgrade: websocket",
        "Connection: Upgrade",
        "Sec-WebSocket-Version: 13",
        "\r\n"
      ].join("\r\n")
    );
    assert.equal(res.status, 400);
    assert.match(res.body, /Invalid WebSocket upgrade/);
  } finally {
    await proxy.close();
  }
});

test("websocket upgrade rejects non-13 Sec-WebSocket-Version (400)", async () => {
  const proxy = await startProxyServer({ listenHost: "127.0.0.1", listenPort: 0, open: true });
  const addr = proxy.server.address();
  assert.ok(addr && typeof addr !== "string");

  try {
    const res = await sendRawUpgradeRequest(
      "127.0.0.1",
      addr.port,
      [
        "GET /tcp?host=127.0.0.1&port=1 HTTP/1.1",
        `Host: 127.0.0.1:${addr.port}`,
        "Upgrade: websocket",
        "Connection: Upgrade",
        "Sec-WebSocket-Version: 12",
        `Sec-WebSocket-Key: ${makeKey()}`,
        "\r\n"
      ].join("\r\n")
    );
    assert.equal(res.status, 400);
    assert.match(res.body, /Invalid WebSocket upgrade/);
  } finally {
    await proxy.close();
  }
});

test("websocket upgrade rejects oversized Upgrade header values (400)", async () => {
  const proxy = await startProxyServer({ listenHost: "127.0.0.1", listenPort: 0, open: true });
  const addr = proxy.server.address();
  assert.ok(addr && typeof addr !== "string");

  try {
    const res = await sendRawUpgradeRequest(
      "127.0.0.1",
      addr.port,
      [
        "GET /tcp?host=127.0.0.1&port=1 HTTP/1.1",
        `Host: 127.0.0.1:${addr.port}`,
        `Upgrade: websocket${"x".repeat(300)}`,
        "Connection: Upgrade",
        "Sec-WebSocket-Version: 13",
        `Sec-WebSocket-Key: ${makeKey()}`,
        "\r\n"
      ].join("\r\n")
    );
    assert.equal(res.status, 400);
    assert.match(res.body, /Invalid WebSocket upgrade/);
  } finally {
    await proxy.close();
  }
});

test("websocket upgrade rejects repeated handshake headers (400)", async () => {
  const proxy = await startProxyServer({ listenHost: "127.0.0.1", listenPort: 0, open: true });
  const addr = proxy.server.address();
  assert.ok(addr && typeof addr !== "string");

  try {
    const res = await sendRawUpgradeRequest(
      "127.0.0.1",
      addr.port,
      [
        "GET /tcp?host=127.0.0.1&port=1 HTTP/1.1",
        `Host: 127.0.0.1:${addr.port}`,
        "Upgrade: websocket",
        "Connection: Upgrade",
        "Sec-WebSocket-Version: 13",
        `Sec-WebSocket-Key: ${makeKey()}`,
        `Sec-WebSocket-Key: ${makeKey()}`,
        "\r\n"
      ].join("\r\n")
    );
    assert.equal(res.status, 400);
    assert.match(res.body, /Invalid WebSocket upgrade/);
  } finally {
    await proxy.close();
  }
});

