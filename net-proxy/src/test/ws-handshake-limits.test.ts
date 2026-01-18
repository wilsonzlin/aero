import test from "node:test";
import assert from "node:assert/strict";
import net from "node:net";
import { PassThrough } from "node:stream";
import { once } from "node:events";
import { WebSocketServer } from "ws";
import { startProxyServer } from "../server";
import { unrefBestEffort } from "../unrefSafe";

async function sendRawUpgradeRequest(
  host: string,
  port: number,
  request: string
): Promise<{ status: number; headers: Record<string, string>; body: string }> {
  return await new Promise((resolve, reject) => {
    const socket = net.connect({ host, port });
    let buf = Buffer.alloc(0);
    let done = false;

    const timeout = setTimeout(() => {
      cleanup();
      reject(new Error("timeout: sendRawUpgradeRequest"));
    }, 2000);
    unrefBestEffort(timeout);

    const cleanup = () => {
      if (done) return;
      done = true;
      clearTimeout(timeout);
      socket.removeAllListeners();
      try {
        socket.destroy();
      } catch {
        // ignore
      }
    };

    const tryParseAndResolve = () => {
      const headerEnd = buf.indexOf("\r\n\r\n");
      if (headerEnd === -1) return;

      const headText = buf.subarray(0, headerEnd).toString("utf8");
      const lines = headText.split("\r\n");
      const statusLine = lines[0] ?? "";
      const m = /^HTTP\/1\.[01]\s+(\d{3})\b/.exec(statusLine);
      const status = m ? Number(m[1]) : 0;

      const headers: Record<string, string> = {};
      for (let i = 1; i < lines.length; i++) {
        const line = lines[i];
        if (!line) continue;
        const idx = line.indexOf(":");
        if (idx === -1) continue;
        const key = line.slice(0, idx).trim().toLowerCase();
        if (!key) continue;
        headers[key] = line.slice(idx + 1).trim();
      }

      const rawLen = headers["content-length"];
      const len = rawLen === undefined ? NaN : Number.parseInt(rawLen, 10);
      if (!Number.isFinite(len) || len < 0) return;

      const bodyStart = headerEnd + 4;
      if (buf.length < bodyStart + len) return;

      const bodyBuf = buf.subarray(bodyStart, bodyStart + len);
      cleanup();
      resolve({ status, headers, body: bodyBuf.toString("utf8") });
    };

    socket.on("error", (err) => {
      cleanup();
      reject(err);
    });

    socket.on("data", (chunk) => {
      const b = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk);
      buf = buf.length === 0 ? b : Buffer.concat([buf, b]);
      tryParseAndResolve();
    });

    socket.on("end", () => tryParseAndResolve());
    socket.on("close", () => {
      if (done) return;
      // Best-effort final parse attempt before failing.
      tryParseAndResolve();
      if (done) return;
      cleanup();
      reject(new Error("sendRawUpgradeRequest: socket closed before full response"));
    });

    socket.write(request);
    socket.end();
  });
}

function makeKey(): string {
  return Buffer.from("dGhlIHNhbXBsZSBub25jZQ==", "base64").toString("base64");
}

async function captureUpgradeResponse(
  server: import("node:http").Server,
  req: import("node:http").IncomingMessage,
): Promise<string> {
  const socket = new PassThrough();
  const chunks: Buffer[] = [];
  socket.on("data", (chunk) => chunks.push(Buffer.from(chunk)));
  const ended = once(socket, "end");

  server.emit("upgrade", req, socket, Buffer.alloc(0));
  await ended;
  try {
    socket.destroy();
  } catch {
    // ignore
  }
  return Buffer.concat(chunks).toString("utf8");
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
        "",
        "",
      ].join("\r\n"),
    );
    assert.equal(res.status, 400);
    assert.equal(res.headers["cache-control"], "no-store");
    assert.match(res.body, /Missing required header: Sec-WebSocket-Key/);
  } finally {
    await proxy.close();
  }
});

test("websocket upgrade returns 500 if req.url getter throws (and server stays alive)", async () => {
  const proxy = await startProxyServer({ listenHost: "127.0.0.1", listenPort: 0, open: true });
  const addr = proxy.server.address();
  assert.ok(addr && typeof addr !== "string");

  try {
    const req = {
      headers: {},
      socket: { remoteAddress: "127.0.0.1" },
    } as unknown as import("node:http").IncomingMessage;

    Object.defineProperty(req, "url", {
      get() {
        throw new Error("boom");
      },
    });

    const res = await captureUpgradeResponse(proxy.server, req);
    assert.ok(res.startsWith("HTTP/1.1 500 "), res);
    assert.ok(res.includes("WebSocket upgrade failed"), res);

    const health = await fetch(`http://127.0.0.1:${addr.port}/healthz`).then((r) => r.json());
    assert.deepEqual(health, { ok: true });
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
        "",
        "",
      ].join("\r\n"),
    );
    assert.equal(res.status, 400);
    assert.equal(res.headers["cache-control"], "no-store");
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
        "",
        "",
      ].join("\r\n"),
    );
    assert.equal(res.status, 400);
    assert.equal(res.headers["cache-control"], "no-store");
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
        "",
        "",
      ].join("\r\n"),
    );
    assert.equal(res.status, 400);
    assert.equal(res.headers["cache-control"], "no-store");
    assert.match(res.body, /Invalid WebSocket upgrade/);
  } finally {
    await proxy.close();
  }
});

test("websocket upgrade returns 500 if ws.handleUpgrade throws (and server stays alive)", async (t) => {
  t.mock.method(WebSocketServer.prototype, "handleUpgrade", () => {
    throw new Error("boom");
  });

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
        "",
        "",
      ].join("\r\n"),
    );
    assert.equal(res.status, 500);
    assert.equal(res.headers["cache-control"], "no-store");
    assert.match(res.body, /WebSocket upgrade failed/);

    // Ensure the proxy is still responsive after the failed upgrade attempt.
    const health = await fetch(`http://127.0.0.1:${addr.port}/healthz`).then((r) => r.json());
    assert.deepEqual(health, { ok: true });
  } finally {
    await proxy.close();
  }
});

test("tcp-mux upgrade rejects missing Sec-WebSocket-Protocol (400)", async () => {
  const proxy = await startProxyServer({ listenHost: "127.0.0.1", listenPort: 0, open: true });
  const addr = proxy.server.address();
  assert.ok(addr && typeof addr !== "string");

  try {
    const res = await sendRawUpgradeRequest(
      "127.0.0.1",
      addr.port,
      [
        "GET /tcp-mux HTTP/1.1",
        `Host: 127.0.0.1:${addr.port}`,
        "Upgrade: websocket",
        "Connection: Upgrade",
        "Sec-WebSocket-Version: 13",
        `Sec-WebSocket-Key: ${makeKey()}`,
        "",
        "",
      ].join("\r\n"),
    );
    assert.equal(res.status, 400);
    assert.equal(res.headers["cache-control"], "no-store");
    assert.match(res.body, /Missing required subprotocol:/);
  } finally {
    await proxy.close();
  }
});

