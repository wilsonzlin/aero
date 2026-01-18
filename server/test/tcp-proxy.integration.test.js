import test from "node:test";
import assert from "node:assert/strict";
import net from "node:net";

import { WebSocket } from "ws";

import { createAeroServer } from "../src/server.js";
import { resolveConfig } from "../src/config.js";
import { decodeServerFrame, encodeClientDataFrame, encodeClientCloseFrame, encodeConnectFrame } from "../src/protocol.js";

function startTcpEchoServer() {
  const server = net.createServer((socket) => {
    socket.on("error", () => {
      // ignore (tests can trigger close races during shutdown)
    });
    socket.on("data", (data) => {
      try {
        socket.write(data);
      } catch {
        try {
          socket.destroy();
        } catch {
          // ignore
        }
      }
    });
  });
  return server;
}

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
  const echoServer = startTcpEchoServer();
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

    ws.send(encodeConnectFrame({ connId: 1, host: "127.0.0.1", port: echoPort }));
    const opened = decodeServerFrame(await nextWsMessage(ws));
    assert.equal(opened.type, "opened");
    assert.equal(opened.connId, 1);
    assert.equal(opened.status, 0);

    ws.send(encodeClientDataFrame({ connId: 1, data: Buffer.from("hello") }));
    const data = decodeServerFrame(await nextWsMessage(ws));
    assert.equal(data.type, "data");
    assert.equal(data.connId, 1);
    assert.equal(data.data.toString("utf8"), "hello");

    ws.send(encodeClientCloseFrame({ connId: 1 }));

    try {
      ws.close();
    } catch {
      try {
        ws.terminate();
      } catch {
        // ignore
      }
    }
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
  let cacheControl;
  try {
    const huge = "a".repeat(9_000);
    const ws = new WebSocket(
      `ws://127.0.0.1:${port}/ws/tcp?token=${encodeURIComponent(token)}&x=${huge}`,
    );
    ws.once("unexpected-response", (_req, res) => {
      statusCode = res.statusCode;
      cacheControl = res.headers["cache-control"];
      res.resume();
    });
    await waitForWsFailure(ws);
    assert.equal(statusCode, 414);
    assert.equal(cacheControl, "no-store");
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
      let statusCode1;
      let cacheControl1;
      const ws = new WebSocket(`ws://127.0.0.1:${port}/ws/tcp?token=${encodeURIComponent(token)}`, {
        headers: { origin: "http://" + "a".repeat(10_000) },
      });
      ws.once("unexpected-response", (_req, res) => {
        statusCode1 = res.statusCode;
        cacheControl1 = res.headers["cache-control"];
        res.resume();
      });
      await waitForWsFailure(ws);
      assert.notEqual(ws.readyState, WebSocket.OPEN);
      assert.equal(statusCode1, 403);
      assert.equal(cacheControl1, "no-store");
    }

    // Oversized Authorization should be treated as invalid (=> 401).
    {
      let statusCode2;
      let cacheControl2;
      const ws = new WebSocket(`ws://127.0.0.1:${port}/ws/tcp`, {
        headers: { origin: "http://ok.example", authorization: "Bearer " + "a".repeat(10_000) },
      });
      ws.once("unexpected-response", (_req, res) => {
        statusCode2 = res.statusCode;
        cacheControl2 = res.headers["cache-control"];
        res.resume();
      });
      await waitForWsFailure(ws);
      assert.equal(statusCode2, 401);
      assert.equal(cacheControl2, "no-store");
    }
  } finally {
    await closeServer(httpServer);
  }
});

test("TCP proxy applies WS backpressure by pausing TCP reads when the client stops reading", { timeout: 10_000 }, async () => {
  const floodChunk = Buffer.alloc(64 * 1024, 0x61);

  /** @type {net.Socket | null} */
  let serverSideSocket = null;
  let didBackpressure = false;

  const floodServer = net.createServer((socket) => {
    serverSideSocket = socket;
    socket.on("error", () => {
      // ignore (tests can trigger close races during shutdown)
    });
  });
  await new Promise((resolve) => floodServer.listen(0, "127.0.0.1", resolve));
  const floodPort = floodServer.address().port;

  const token = "test-token";
  const config = resolveConfig({
    host: "127.0.0.1",
    port: 0,
    tokens: [token],
    allowPrivateRanges: true,
    allowHosts: [{ kind: "exact", value: "127.0.0.1" }],
    allowPorts: [{ start: floodPort, end: floodPort }],
    // Make the backpressure trigger quickly.
    wsBackpressureHighWatermarkBytes: 128 * 1024,
    wsBackpressureLowWatermarkBytes: 64 * 1024,
  });

  const { httpServer } = createAeroServer(config);
  const port = await listen(httpServer);

  try {
    const ws = new WebSocket(`ws://127.0.0.1:${port}/ws/tcp?token=${encodeURIComponent(token)}`);
    await waitForWsOpen(ws);

    ws.send(encodeConnectFrame({ connId: 1, host: "127.0.0.1", port: floodPort }));
    const opened = decodeServerFrame(await nextWsMessage(ws));
    assert.equal(opened.type, "opened");
    assert.equal(opened.status, 0);

    // Stop the WS client from reading so the server starts buffering.
    ws._socket?.pause?.();

    // Wait for the TCP server side to be connected through the proxy.
    const start = Date.now();
    while (!serverSideSocket && Date.now() - start < 2000) {
      await new Promise((r) => setTimeout(r, 10));
    }
    assert.ok(serverSideSocket, "expected upstream TCP connection");

    // Flood the proxy until the upstream sees backpressure (proxy paused reads).
    const floodUntilBackpressure = () => {
      if (!serverSideSocket) return;
      while (true) {
        const ok = serverSideSocket.write(floodChunk);
        if (!ok) {
          didBackpressure = true;
          return;
        }
      }
    };
    floodUntilBackpressure();

    const deadline = Date.now() + 2000;
    while (!didBackpressure && Date.now() < deadline) {
      await new Promise((r) => setTimeout(r, 10));
      floodUntilBackpressure();
    }
    assert.equal(didBackpressure, true, "expected upstream TCP writes to hit backpressure when WS stops reading");

    try {
      ws.terminate();
    } catch {
      // ignore
    }
    await new Promise((resolve) => ws.once("close", resolve));
  } finally {
    await closeServer(httpServer);
    await new Promise((resolve) => floodServer.close(resolve));
  }
});

