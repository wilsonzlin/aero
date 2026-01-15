import assert from "node:assert/strict";
import http from "node:http";
import net from "node:net";
import { once } from "node:events";
import { PassThrough } from "node:stream";
import { describe, it } from "node:test";

import { handleTcpProxyUpgrade } from "../src/routes/tcpProxy.js";

async function listen(server: http.Server | net.Server, host?: string): Promise<number> {
  server.listen(0, host);
  await once(server, "listening");
  const addr = server.address();
  if (addr && typeof addr === "object") return addr.port;
  throw new Error("Expected server to bind to an ephemeral port");
}

async function closeServer(server: http.Server | net.Server): Promise<void> {
  try {
    // Ensure tests don't hang on leaked upgrade sockets.
    (server as unknown as { closeAllConnections?: () => void }).closeAllConnections?.();
    (server as unknown as { closeIdleConnections?: () => void }).closeIdleConnections?.();
    server.close();
  } catch (err) {
    const code = (err as { code?: unknown } | null)?.code;
    if (code === "ERR_SERVER_NOT_RUNNING") return;
    throw err;
  }
  await once(server, "close");
}

async function captureUpgradeResponse(run: (socket: PassThrough) => void): Promise<string> {
  const socket = new PassThrough();
  const chunks: Buffer[] = [];
  socket.on("data", (chunk) => chunks.push(Buffer.from(chunk)));
  const ended = once(socket, "end");
  run(socket);
  await ended;
  return Buffer.concat(chunks).toString("utf8");
}

function openWebSocket(url: string): Promise<WebSocket> {
  const ws = new WebSocket(url);
  ws.binaryType = "arraybuffer";
  return new Promise((resolve, reject) => {
    const onOpen = () => {
      cleanup();
      resolve(ws);
    };
    const onError = () => {
      cleanup();
      reject(new Error("WebSocket connection failed"));
    };
    const cleanup = () => {
      ws.removeEventListener("open", onOpen);
      ws.removeEventListener("error", onError);
    };
    ws.addEventListener("open", onOpen);
    ws.addEventListener("error", onError);
  });
}

function nextMessage(ws: WebSocket): Promise<ArrayBuffer> {
  return new Promise((resolve) => {
    ws.addEventListener(
      "message",
      (event) => {
        resolve(event.data as ArrayBuffer);
      },
      { once: true },
    );
  });
}

async function closeWebSocket(ws: WebSocket): Promise<void> {
  if (ws.readyState === WebSocket.CLOSED) return;
  ws.close();
  await new Promise<void>((resolve) => ws.addEventListener("close", () => resolve(), { once: true }));
}

describe("tcpProxy route", () => {
  it("rejects overly long request URLs (414)", async () => {
    const req = {
      url: `/tcp?${"a".repeat(9000)}`,
      headers: {},
      socket: { remoteAddress: "127.0.0.1" },
    } as unknown as http.IncomingMessage;

    const res = await captureUpgradeResponse((socket) => {
      handleTcpProxyUpgrade(req, socket, Buffer.alloc(0));
    });
    assert.ok(res.startsWith("HTTP/1.1 414 "));
  });

  it("rejects non-WebSocket requests early (400)", async () => {
    const req = {
      url: "/tcp?v=1&host=127.0.0.1&port=1",
      headers: {},
      socket: { remoteAddress: "127.0.0.1" },
    } as unknown as http.IncomingMessage;

    const res = await captureUpgradeResponse((socket) => {
      handleTcpProxyUpgrade(req, socket, Buffer.alloc(0));
    });
    assert.ok(res.startsWith("HTTP/1.1 400 "));
    assert.ok(res.includes("Invalid WebSocket upgrade"));
  });

  it("proxies TCP using host+port query parameters", async () => {
    const echoServer = net.createServer((socket) => socket.on("data", (data) => socket.write(data)));
    const proxyServer = http.createServer();
    let ws: WebSocket | null = null;

    try {
      const echoPort = await listen(echoServer, "127.0.0.1");

      proxyServer.on("upgrade", (req, socket, head) => {
        handleTcpProxyUpgrade(req, socket, head, {
          createConnection: (() => net.createConnection({ host: "127.0.0.1", port: echoPort })) as typeof net.createConnection,
        });
      });
      const proxyPort = await listen(proxyServer, "127.0.0.1");

      ws = await openWebSocket(`ws://127.0.0.1:${proxyPort}/tcp?v=1&host=8.8.8.8&port=${echoPort}`);

      const payload = Buffer.from([1, 2, 3, 4]);
      ws.send(payload);

      const message = await nextMessage(ws);
      assert.ok(message instanceof ArrayBuffer);
      assert.deepEqual(Buffer.from(message), payload);
    } finally {
      if (ws) await closeWebSocket(ws);
      await closeServer(proxyServer);
      await closeServer(echoServer);
    }
  });

  it("proxies TCP using target query parameter (and prefers it when both forms are present)", async () => {
    const echoServer = net.createServer((socket) => socket.on("data", (data) => socket.write(data)));
    const proxyServer = http.createServer();
    let ws: WebSocket | null = null;

    try {
      const echoPort = await listen(echoServer, "127.0.0.1");

      proxyServer.on("upgrade", (req, socket, head) => {
        handleTcpProxyUpgrade(req, socket, head, {
          createConnection: (() => net.createConnection({ host: "127.0.0.1", port: echoPort })) as typeof net.createConnection,
        });
      });
      const proxyPort = await listen(proxyServer, "127.0.0.1");

      ws = await openWebSocket(
        `ws://127.0.0.1:${proxyPort}/tcp?v=1&target=8.8.8.8:${echoPort}&host=256.256.256.256&port=1`,
      );

      const payload = Buffer.from("hello");
      ws.send(payload);

      const message = await nextMessage(ws);
      assert.ok(message instanceof ArrayBuffer);
      assert.deepEqual(Buffer.from(message), payload);
    } finally {
      if (ws) await closeWebSocket(ws);
      await closeServer(proxyServer);
      await closeServer(echoServer);
    }
  });

  it("rejects unsupported protocol versions", async () => {
    const proxyServer = http.createServer();
    proxyServer.on("upgrade", (req, socket, head) => {
      handleTcpProxyUpgrade(req, socket, head);
    });
    const proxyPort = await listen(proxyServer, "127.0.0.1");

    try {
      await assert.rejects(
        () => openWebSocket(`ws://127.0.0.1:${proxyPort}/tcp?v=2&host=127.0.0.1&port=1`),
        /WebSocket connection failed/,
      );
    } finally {
      await closeServer(proxyServer);
    }
  });

  it("allows dialing loopback targets when allowPrivateIps is enabled", async () => {
    const echoServer = net.createServer((socket) => socket.on("data", (data) => socket.write(data)));
    const proxyServer = http.createServer();
    let ws: WebSocket | null = null;

    try {
      const echoPort = await listen(echoServer, "127.0.0.1");

      proxyServer.on("upgrade", (req, socket, head) => {
        handleTcpProxyUpgrade(req, socket, head, { allowPrivateIps: true });
      });
      const proxyPort = await listen(proxyServer, "127.0.0.1");

      ws = await openWebSocket(`ws://127.0.0.1:${proxyPort}/tcp?v=1&host=127.0.0.1&port=${echoPort}`);

      const payload = Buffer.from("ping");
      ws.send(payload);
      const message = await nextMessage(ws);
      assert.deepEqual(Buffer.from(message), payload);
    } finally {
      if (ws) await closeWebSocket(ws);
      await closeServer(proxyServer);
      await closeServer(echoServer);
    }
  });

  it("rejects dialing loopback targets when allowPrivateIps is disabled", async () => {
    const echoServer = net.createServer((socket) => socket.on("data", (data) => socket.write(data)));
    const echoPort = await listen(echoServer, "127.0.0.1");

    const proxyServer = http.createServer();
    proxyServer.on("upgrade", (req, socket, head) => {
      handleTcpProxyUpgrade(req, socket, head, { allowPrivateIps: false });
    });
    const proxyPort = await listen(proxyServer, "127.0.0.1");

    try {
      await assert.rejects(
        () => openWebSocket(`ws://127.0.0.1:${proxyPort}/tcp?v=1&host=127.0.0.1&port=${echoPort}`),
        /WebSocket connection failed/,
      );
    } finally {
      await closeServer(proxyServer);
      await closeServer(echoServer);
    }
  });
});
