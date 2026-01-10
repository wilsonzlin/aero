import assert from "node:assert/strict";
import http from "node:http";
import net from "node:net";
import { once } from "node:events";
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
  server.close();
  await once(server, "close");
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
  it("proxies TCP using host+port query parameters", async () => {
    const echoServer = net.createServer((socket) => socket.on("data", (data) => socket.write(data)));
    const echoPort = await listen(echoServer, "127.0.0.1");

    const proxyServer = http.createServer();
    proxyServer.on("upgrade", (req, socket, head) => {
      handleTcpProxyUpgrade(req, socket, head, {
        createConnection: (() => net.createConnection({ host: "127.0.0.1", port: echoPort })) as typeof net.createConnection,
      });
    });
    const proxyPort = await listen(proxyServer, "127.0.0.1");

    const ws = await openWebSocket(
      `ws://127.0.0.1:${proxyPort}/tcp?host=8.8.8.8&port=${echoPort}`,
    );

    try {
      const payload = Buffer.from([1, 2, 3, 4]);
      ws.send(payload);

      const message = await nextMessage(ws);
      assert.ok(message instanceof ArrayBuffer);
      assert.deepEqual(Buffer.from(message), payload);
    } finally {
      await closeWebSocket(ws);
      await closeServer(proxyServer);
      await closeServer(echoServer);
    }
  });

  it("proxies TCP using target query parameter (and prefers it when both forms are present)", async () => {
    const echoServer = net.createServer((socket) => socket.on("data", (data) => socket.write(data)));
    const echoPort = await listen(echoServer, "127.0.0.1");

    const proxyServer = http.createServer();
    proxyServer.on("upgrade", (req, socket, head) => {
      handleTcpProxyUpgrade(req, socket, head, {
        createConnection: (() => net.createConnection({ host: "127.0.0.1", port: echoPort })) as typeof net.createConnection,
      });
    });
    const proxyPort = await listen(proxyServer, "127.0.0.1");

    const ws = await openWebSocket(
      `ws://127.0.0.1:${proxyPort}/tcp?target=8.8.8.8:${echoPort}&host=256.256.256.256&port=1`,
    );

    try {
      const payload = Buffer.from("hello");
      ws.send(payload);

      const message = await nextMessage(ws);
      assert.ok(message instanceof ArrayBuffer);
      assert.deepEqual(Buffer.from(message), payload);
    } finally {
      await closeWebSocket(ws);
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
});
