import test from "node:test";
import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import net from "node:net";
import dgram from "node:dgram";
import { WebSocket } from "ws";
import { startProxyServer } from "../server";
import { unrefBestEffort } from "../unrefSafe";

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
    try {
      server.send(msg, rinfo.port, rinfo.address);
    } catch {
      // ignore (tests can race with shutdown)
    }
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
    unrefBestEffort(timeout);
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
    unrefBestEffort(timeout);
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
    unrefBestEffort(timeout);
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
    const ws = await openWebSocket(`ws://127.0.0.1:${proxyAddr.port}/tcp?v=1&host=127.0.0.1&port=${echoServer.port}`);
    const payload = Buffer.from([0, 1, 2, 3, 4, 5, 255]);
    const receivedPromise = waitForBinaryMessage(ws);
    try {
      ws.send(payload);
    } catch {
      // If the websocket closed unexpectedly, fail via the normal timeout path.
    }

    const received = await receivedPromise;
    assert.deepEqual(received, payload);

    const closePromise = waitForClose(ws);
    try {
      ws.close(1000, "done");
    } catch {
      // ignore close races
    }
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
    const ws = await openWebSocket(`ws://127.0.0.1:${proxyAddr.port}/tcp?v=1&target=127.0.0.1:${echoServer.port}`);
    const payload = Buffer.from("hello");
    const receivedPromise = waitForBinaryMessage(ws);
    try {
      ws.send(payload);
    } catch {
      // ignore close races; test will fail via timeout if needed.
    }

    const received = await receivedPromise;
    assert.deepEqual(received, payload);

    const closePromise = waitForClose(ws);
    try {
      ws.close(1000, "done");
    } catch {
      // ignore close races
    }
    await closePromise;
  } finally {
    await proxy.close();
    await echoServer.close();
  }
});

test("tcp relay enforces socket-level buffering cap with 1011 close", async () => {
  let createdResolve: (() => void) | null = null;
  const created = new Promise<void>((resolve) => {
    createdResolve = resolve;
  });

  const proxy = await startProxyServer({
    listenHost: "127.0.0.1",
    listenPort: 0,
    open: true,
    maxTcpBufferedBytesPerConn: 32,
    createTcpConnection: () => {
      class FakeTcpSocket extends EventEmitter {
        writableLength = 0;
        setNoDelay() {}
        pause() {}
        resume() {}
        write(chunk: unknown) {
          const buf = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk as any);
          this.writableLength += buf.length;
          return true;
        }
        destroy() {
          queueMicrotask(() => this.emit("close"));
        }
      }

      createdResolve?.();
      const socket = new FakeTcpSocket();
      queueMicrotask(() => socket.emit("connect"));
      return socket as unknown as net.Socket;
    }
  });
  const proxyAddr = proxy.server.address();
  assert.ok(proxyAddr && typeof proxyAddr !== "string");

  let ws: WebSocket | null = null;
  try {
    ws = await openWebSocket(`ws://127.0.0.1:${proxyAddr.port}/tcp?v=1&host=127.0.0.1&port=80`);

    await Promise.race([
      created,
      new Promise((_, reject) => {
        const timeout = setTimeout(() => reject(new Error("timeout waiting for createTcpConnection")), 2_000);
        unrefBestEffort(timeout);
      })
    ]);

    const closePromise = waitForClose(ws);
    try {
      ws.send(Buffer.from("x".repeat(1024), "utf8"));
    } catch {
      // ignore close races
    }

    const closed = await closePromise;
    assert.equal(closed.code, 1011);
    assert.equal(closed.reason, "TCP buffered too much data");
  } finally {
    if (ws && ws.readyState !== ws.CLOSED) {
      ws.terminate();
      await waitForClose(ws).catch(() => {
        // ignore
      });
    }
    await proxy.close();
  }
});

test("tcp relay closes with 1011 if tcpSocket.writableLength getter throws", async () => {
  let createdResolve: (() => void) | null = null;
  const created = new Promise<void>((resolve) => {
    createdResolve = resolve;
  });

  const proxy = await startProxyServer({
    listenHost: "127.0.0.1",
    listenPort: 0,
    open: true,
    maxTcpBufferedBytesPerConn: 32,
    createTcpConnection: () => {
      class FakeTcpSocket extends EventEmitter {
        setNoDelay() {}
        pause() {}
        resume() {}
        get writableLength() {
          throw new Error("boom");
        }
        write(_chunk: unknown) {
          return true;
        }
        destroy() {
          queueMicrotask(() => this.emit("close"));
        }
      }

      createdResolve?.();
      const socket = new FakeTcpSocket();
      queueMicrotask(() => socket.emit("connect"));
      return socket as unknown as net.Socket;
    }
  });
  const proxyAddr = proxy.server.address();
  assert.ok(proxyAddr && typeof proxyAddr !== "string");

  let ws: WebSocket | null = null;
  try {
    ws = await openWebSocket(`ws://127.0.0.1:${proxyAddr.port}/tcp?v=1&host=127.0.0.1&port=80`);

    await Promise.race([
      created,
      new Promise((_, reject) => {
        const timeout = setTimeout(() => reject(new Error("timeout waiting for createTcpConnection")), 2_000);
        unrefBestEffort(timeout);
      })
    ]);

    const closePromise = waitForClose(ws);
    try {
      ws.send(Buffer.from("a"));
    } catch {
      // ignore close races
    }

    const closed = await closePromise;
    assert.equal(closed.code, 1011);
    assert.equal(closed.reason, "TCP buffered too much data");
  } finally {
    if (ws && ws.readyState !== ws.CLOSED) {
      ws.terminate();
      await waitForClose(ws).catch(() => {
        // ignore
      });
    }
    await proxy.close();
  }
});

test("tcp relay denies private targets by default", async () => {
  const echoServer = await startTcpEchoServer();
  const proxy = await startProxyServer({ listenHost: "127.0.0.1", listenPort: 0, open: false, allow: "" });
  const proxyAddr = proxy.server.address();
  assert.ok(proxyAddr && typeof proxyAddr !== "string");

  try {
    const ws = new WebSocket(`ws://127.0.0.1:${proxyAddr.port}/tcp?v=1&host=127.0.0.1&port=${echoServer.port}`);
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
    const ws = await openWebSocket(`ws://127.0.0.1:${proxyAddr.port}/tcp?v=1&host=127.0.0.1&port=${echoServer.port}`);
    const payload = Buffer.from("allowlisted");
    const receivedPromise = waitForBinaryMessage(ws);
    try {
      ws.send(payload);
    } catch {
      // ignore close races; test will fail via timeout if needed.
    }

    const received = await receivedPromise;
    assert.deepEqual(received, payload);

    const closePromise = waitForClose(ws);
    try {
      ws.close(1000, "done");
    } catch {
      // ignore close races
    }
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
    const ws = new WebSocket(`ws://127.0.0.1:${proxyAddr.port}/tcp?v=1&host=127.0.0.1&port=${echoServer.port}`);
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
    try {
      ws.send(payload);
    } catch {
      // ignore close races; test will fail via timeout if needed.
    }

    const received = await receivedPromise;
    assert.deepEqual(received, payload);

    const closePromise = waitForClose(ws);
    try {
      ws.close(1000, "done");
    } catch {
      // ignore close races
    }
    await closePromise;
  } finally {
    await proxy.close();
    await udpServer.close();
  }
});
