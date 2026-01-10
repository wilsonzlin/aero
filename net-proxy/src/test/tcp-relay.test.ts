import test from "node:test";
import assert from "node:assert/strict";
import net from "node:net";
import dgram from "node:dgram";
import { WebSocket } from "ws";
import { startProxyServer } from "../server";

async function startTcpEchoServer(): Promise<{ port: number; close: () => Promise<void> }> {
  const server = net.createServer((socket) => {
    socket.on("error", () => {
      // Ignore socket errors for test shutdown.
    });
    socket.pipe(socket);
  });

  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  const addr = server.address();
  assert.ok(addr && typeof addr !== "string");

  return {
    port: addr.port,
    close: async () => new Promise<void>((resolve, reject) => server.close((err) => (err ? reject(err) : resolve())))
  };
}

async function startUdpEchoServer(): Promise<{ port: number; close: () => Promise<void> }> {
  const server = dgram.createSocket("udp4");

  server.on("message", (msg, rinfo) => {
    server.send(msg, rinfo.port, rinfo.address);
  });

  await new Promise<void>((resolve) => server.bind(0, "127.0.0.1", resolve));
  const addr = server.address();
  assert.ok(typeof addr !== "string");

  return {
    port: addr.port,
    close: async () =>
      new Promise<void>((resolve) => {
        server.close(() => resolve());
      })
  };
}

async function openWebSocket(url: string): Promise<WebSocket> {
  const ws = new WebSocket(url);
  await new Promise<void>((resolve, reject) => {
    const timeout = setTimeout(() => reject(new Error("timeout waiting for websocket open")), 2_000);
    timeout.unref();
    ws.once("open", () => {
      clearTimeout(timeout);
      resolve();
    });
    ws.once("error", (err) => {
      clearTimeout(timeout);
      reject(err);
    });
  });
  return ws;
}

async function waitForBinaryMessage(ws: WebSocket, timeoutMs = 2_000): Promise<Buffer> {
  return new Promise<Buffer>((resolve, reject) => {
    const timeout = setTimeout(() => reject(new Error("timeout waiting for message")), timeoutMs);
    timeout.unref();
    ws.once("message", (data, isBinary) => {
      clearTimeout(timeout);
      assert.equal(isBinary, true);
      resolve(Buffer.isBuffer(data) ? data : Buffer.from(data as ArrayBuffer));
    });
    ws.once("error", (err) => {
      clearTimeout(timeout);
      reject(err);
    });
  });
}

async function waitForClose(ws: WebSocket, timeoutMs = 2_000): Promise<{ code: number; reason: string }> {
  return new Promise((resolve, reject) => {
    const timeout = setTimeout(() => reject(new Error("timeout waiting for close")), timeoutMs);
    timeout.unref();
    ws.once("close", (code, reason) => {
      clearTimeout(timeout);
      resolve({ code, reason: reason.toString() });
    });
    ws.once("error", (err) => {
      clearTimeout(timeout);
      reject(err);
    });
  });
}

test("tcp relay echoes bytes roundtrip", async () => {
  const echoServer = await startTcpEchoServer();
  const proxy = await startProxyServer({ listenHost: "127.0.0.1", listenPort: 0, open: true });
  const proxyAddr = proxy.server.address();
  assert.ok(proxyAddr && typeof proxyAddr !== "string");

  try {
    const ws = await openWebSocket(`ws://127.0.0.1:${proxyAddr.port}/tcp?host=127.0.0.1&port=${echoServer.port}`);
    const payload = Buffer.from([0, 1, 2, 3, 4, 5, 255]);
    const receivedPromise = waitForBinaryMessage(ws);
    ws.send(payload);

    const received = await receivedPromise;
    assert.deepEqual(received, payload);

    const closePromise = waitForClose(ws);
    ws.close(1000, "done");
    await closePromise;
  } finally {
    await proxy.close();
    await echoServer.close();
  }
});

test("tcp relay supports target=host:port alias", async () => {
  const echoServer = await startTcpEchoServer();
  const proxy = await startProxyServer({ listenHost: "127.0.0.1", listenPort: 0, open: true });
  const proxyAddr = proxy.server.address();
  assert.ok(proxyAddr && typeof proxyAddr !== "string");

  try {
    const ws = await openWebSocket(`ws://127.0.0.1:${proxyAddr.port}/tcp?target=127.0.0.1:${echoServer.port}`);
    const payload = Buffer.from("hello");
    const receivedPromise = waitForBinaryMessage(ws);
    ws.send(payload);

    const received = await receivedPromise;
    assert.deepEqual(received, payload);

    const closePromise = waitForClose(ws);
    ws.close(1000, "done");
    await closePromise;
  } finally {
    await proxy.close();
    await echoServer.close();
  }
});

test("tcp relay denies private targets by default", async () => {
  const echoServer = await startTcpEchoServer();
  const proxy = await startProxyServer({ listenHost: "127.0.0.1", listenPort: 0, open: false, allow: "" });
  const proxyAddr = proxy.server.address();
  assert.ok(proxyAddr && typeof proxyAddr !== "string");

  try {
    const ws = new WebSocket(`ws://127.0.0.1:${proxyAddr.port}/tcp?host=127.0.0.1&port=${echoServer.port}`);
    const closed = await waitForClose(ws);
    assert.equal(closed.code, 1008);
  } finally {
    await proxy.close();
    await echoServer.close();
  }
});

test("tcp relay allowlist permits private targets", async () => {
  const echoServer = await startTcpEchoServer();
  const proxy = await startProxyServer({
    listenHost: "127.0.0.1",
    listenPort: 0,
    open: false,
    allow: "127.0.0.1:*"
  });
  const proxyAddr = proxy.server.address();
  assert.ok(proxyAddr && typeof proxyAddr !== "string");

  try {
    const ws = await openWebSocket(`ws://127.0.0.1:${proxyAddr.port}/tcp?host=127.0.0.1&port=${echoServer.port}`);
    const payload = Buffer.from("allowlisted");
    const receivedPromise = waitForBinaryMessage(ws);
    ws.send(payload);

    const received = await receivedPromise;
    assert.deepEqual(received, payload);

    const closePromise = waitForClose(ws);
    ws.close(1000, "done");
    await closePromise;
  } finally {
    await proxy.close();
    await echoServer.close();
  }
});

test("domain wildcard allowlist still blocks private targets (DNS rebinding mitigation)", async () => {
  const echoServer = await startTcpEchoServer();
  const proxy = await startProxyServer({
    listenHost: "127.0.0.1",
    listenPort: 0,
    open: false,
    allow: "*:*"
  });
  const proxyAddr = proxy.server.address();
  assert.ok(proxyAddr && typeof proxyAddr !== "string");

  try {
    const ws = new WebSocket(`ws://127.0.0.1:${proxyAddr.port}/tcp?host=127.0.0.1&port=${echoServer.port}`);
    const closed = await waitForClose(ws);
    assert.equal(closed.code, 1008);
  } finally {
    await proxy.close();
    await echoServer.close();
  }
});

test("udp relay echoes datagrams roundtrip", async () => {
  const udpServer = await startUdpEchoServer();
  const proxy = await startProxyServer({ listenHost: "127.0.0.1", listenPort: 0, open: true });
  const proxyAddr = proxy.server.address();
  assert.ok(proxyAddr && typeof proxyAddr !== "string");

  try {
    const ws = await openWebSocket(`ws://127.0.0.1:${proxyAddr.port}/udp?host=127.0.0.1&port=${udpServer.port}`);
    const payload = Buffer.from([9, 8, 7, 6]);
    const receivedPromise = waitForBinaryMessage(ws);
    ws.send(payload);

    const received = await receivedPromise;
    assert.deepEqual(received, payload);

    const closePromise = waitForClose(ws);
    ws.close(1000, "done");
    await closePromise;
  } finally {
    await proxy.close();
    await udpServer.close();
  }
});
