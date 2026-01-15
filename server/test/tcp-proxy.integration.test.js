import test from "node:test";
import assert from "node:assert/strict";
import net from "node:net";

import { WebSocket } from "ws";

import { createAeroServer } from "../src/server.js";
import { resolveConfig } from "../src/config.js";
import { decodeServerFrame, encodeClientDataFrame, encodeClientCloseFrame, encodeConnectFrame } from "../src/protocol.js";

async function listen(server) {
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const address = server.address();
  return address.port;
}

async function closeServer(server) {
  await new Promise((resolve) => server.close(resolve));
}

function waitForWsOpen(ws) {
  return new Promise((resolve, reject) => {
    ws.once("open", resolve);
    ws.once("error", reject);
  });
}

function waitForWsFailure(ws) {
  return new Promise((resolve) => {
    ws.once("error", () => resolve());
    ws.once("unexpected-response", () => resolve());
    ws.once("close", () => resolve());
  });
}

function nextWsMessage(ws) {
  return new Promise((resolve, reject) => {
    ws.once("message", (data, isBinary) => {
      if (!isBinary) reject(new Error("Expected binary message"));
      else resolve(Buffer.isBuffer(data) ? data : Buffer.from(data));
    });
    ws.once("error", reject);
  });
}

test("TCP proxy can connect to a local echo server and relay data", { timeout: 10_000 }, async () => {
  const echoServer = net.createServer((socket) => socket.pipe(socket));
  await new Promise((resolve) => echoServer.listen(0, "127.0.0.1", resolve));
  const echoPort = echoServer.address().port;

  const token = "test-token";
  const config = resolveConfig({
    host: "127.0.0.1",
    port: 0,
    tokens: [token],
    allowPrivateRanges: true,
    allowHosts: [{ kind: "exact", value: "127.0.0.1" }],
    allowPorts: [{ start: echoPort, end: echoPort }],
    maxTcpConnectionsTotal: 10,
    maxTcpConnectionsPerWs: 2,
  });

  const { httpServer } = createAeroServer(config);
  const port = await listen(httpServer);

  try {
    const ws = new WebSocket(`ws://127.0.0.1:${port}/ws/tcp?token=${encodeURIComponent(token)}`);
    await waitForWsOpen(ws);

    ws.send(encodeConnectFrame({ connId: 1, host: "127.0.0.1", port: echoPort }), { binary: true });
    const opened = decodeServerFrame(await nextWsMessage(ws));
    assert.equal(opened.type, "opened");
    assert.equal(opened.connId, 1);
    assert.equal(opened.status, 0);

    ws.send(encodeClientDataFrame({ connId: 1, data: Buffer.from("hello") }), { binary: true });
    const data = decodeServerFrame(await nextWsMessage(ws));
    assert.equal(data.type, "data");
    assert.equal(data.connId, 1);
    assert.equal(data.data.toString("utf8"), "hello");

    ws.send(encodeClientCloseFrame({ connId: 1 }), { binary: true });

    ws.close();
    await new Promise((resolve) => ws.once("close", resolve));
  } finally {
    await closeServer(httpServer);
    await new Promise((resolve) => echoServer.close(resolve));
  }
});

test("upgrade rejects overly long request targets with 414", async () => {
  const token = "test-token";
  const config = resolveConfig({
    host: "127.0.0.1",
    port: 0,
    tokens: [token],
    allowPrivateRanges: true,
    allowHosts: [{ kind: "exact", value: "127.0.0.1" }],
    allowPorts: [{ start: 1, end: 65535 }],
  });

  const { httpServer } = createAeroServer(config);
  const port = await listen(httpServer);

  let statusCode;
  try {
    const huge = "a".repeat(9_000);
    const ws = new WebSocket(
      `ws://127.0.0.1:${port}/ws/tcp?token=${encodeURIComponent(token)}&x=${huge}`,
    );
    ws.once("unexpected-response", (_req, res) => {
      statusCode = res.statusCode;
      res.resume();
    });
    await waitForWsFailure(ws);
    assert.equal(statusCode, 414);
  } finally {
    await closeServer(httpServer);
  }
});

test("upgrade caps oversized Origin and Authorization headers", async () => {
  const token = "test-token";
  const config = resolveConfig({
    host: "127.0.0.1",
    port: 0,
    tokens: [token],
    allowedOrigins: ["http://ok.example"],
    allowPrivateRanges: true,
    allowHosts: [{ kind: "exact", value: "127.0.0.1" }],
    allowPorts: [{ start: 1, end: 65535 }],
  });

  const { httpServer } = createAeroServer(config);
  const port = await listen(httpServer);

  try {
    // Oversized Origin should be treated as invalid and rejected.
    {
      const ws = new WebSocket(`ws://127.0.0.1:${port}/ws/tcp?token=${encodeURIComponent(token)}`, {
        headers: { origin: "http://" + "a".repeat(10_000) },
      });
      await waitForWsFailure(ws);
      assert.notEqual(ws.readyState, WebSocket.OPEN);
    }

    // Oversized Authorization should be treated as invalid (=> 401).
    {
      let statusCode2;
      const ws = new WebSocket(`ws://127.0.0.1:${port}/ws/tcp`, {
        headers: { origin: "http://ok.example", authorization: "Bearer " + "a".repeat(10_000) },
      });
      ws.once("unexpected-response", (_req, res) => {
        statusCode2 = res.statusCode;
        res.resume();
      });
      await waitForWsFailure(ws);
      assert.equal(statusCode2, 401);
    }
  } finally {
    await closeServer(httpServer);
  }
});

