import test from "node:test";
import assert from "node:assert/strict";
import { PassThrough } from "node:stream";
import { once } from "node:events";
import { WebSocket, WebSocketServer } from "ws";

import { createAeroServer } from "../src/server.js";
import { resolveConfig } from "../src/config.js";

async function listen(server) {
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const address = server.address();
  return address.port;
}

async function closeServer(server) {
  await new Promise((resolve) => server.close(resolve));
}

function waitForWsFailure(ws) {
  return new Promise((resolve) => {
    ws.once("error", () => resolve());
    ws.once("unexpected-response", () => resolve());
    ws.once("close", () => resolve());
  });
}

async function captureUpgradeResponse(httpServer, req) {
  const socket = new PassThrough();
  const chunks = [];
  socket.on("data", (chunk) => chunks.push(Buffer.from(chunk)));
  const ended = once(socket, "end");

  httpServer.emit("upgrade", req, socket, Buffer.alloc(0));
  await ended;
  try {
    socket.destroy();
  } catch {
    // ignore
  }
  return Buffer.concat(chunks).toString("utf8");
}

test("upgrade: returns 500 when req.url getter throws (and server stays alive)", async () => {
  const token = "test-token";
  const config = resolveConfig({
    host: "127.0.0.1",
    port: 0,
    tokens: [token],
  });

  const { httpServer } = createAeroServer(config);
  const port = await listen(httpServer);

  try {
    const req = {
      headers: {},
      socket: { remoteAddress: "127.0.0.1" },
    };
    Object.defineProperty(req, "url", {
      get() {
        throw new Error("boom");
      },
    });

    const res = await captureUpgradeResponse(httpServer, req);
    assert.ok(res.startsWith("HTTP/1.1 500 Internal Server Error\r\n"), res);
    assert.ok(res.includes("WebSocket upgrade failed\n"), res);

    const health = await fetch(`http://127.0.0.1:${port}/healthz`);
    assert.equal(health.status, 200);
    assert.equal(await health.text(), "ok");
  } finally {
    await closeServer(httpServer);
  }
});

test("upgrade: returns 500 when ws handleUpgrade throws (and server stays alive)", async (t) => {
  const token = "test-token";
  const config = resolveConfig({
    host: "127.0.0.1",
    port: 0,
    tokens: [token],
  });

  const { httpServer } = createAeroServer(config);
  const port = await listen(httpServer);

  t.mock.method(WebSocketServer.prototype, "handleUpgrade", () => {
    throw new Error("boom");
  });

  try {
    let statusCode;
    const ws = new WebSocket(`ws://127.0.0.1:${port}/ws/tcp?token=${encodeURIComponent(token)}`);
    ws.once("unexpected-response", (_req, res) => {
      statusCode = res.statusCode;
      res.resume();
    });

    await waitForWsFailure(ws);
    assert.equal(statusCode, 500);

    const health = await fetch(`http://127.0.0.1:${port}/healthz`);
    assert.equal(health.status, 200);
    assert.equal(await health.text(), "ok");
  } finally {
    await closeServer(httpServer);
  }
});

