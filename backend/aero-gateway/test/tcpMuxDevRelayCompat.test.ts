import assert from "node:assert/strict";
import net from "node:net";
import { once } from "node:events";
import { describe, it } from "node:test";

import { createProxyServer } from "../../../tools/net-proxy-server/src/server.js";
import { WebSocketTcpMuxProxyClient } from "../../../web/src/net/tcpMuxProxy.ts";
import { unrefBestEffort } from "../../../src/unref_safe.js";

async function listen(server: net.Server, host = "127.0.0.1"): Promise<number> {
  server.listen(0, host);
  await once(server, "listening");
  const addr = server.address();
  if (addr && typeof addr === "object") return addr.port;
  throw new Error("Expected server to bind to an ephemeral port");
}

async function closeServer(server: net.Server): Promise<void> {
  server.close();
  await once(server, "close");
}

async function withTimeout<T>(promise: Promise<T>, timeoutMs: number, message: string): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | null = null;
  const timeout = new Promise<never>((_, reject) => {
    timer = setTimeout(() => reject(new Error(message)), timeoutMs);
    unrefBestEffort(timer);
  });
  try {
    return await Promise.race([promise, timeout]);
  } finally {
    if (timer) clearTimeout(timer);
  }
}

describe("tcp-mux dev relay compatibility", () => {
  it("WebSocketTcpMuxProxyClient can connect to tools/net-proxy-server with ?token= auth", async () => {
    const echoServer = net.createServer((socket) => socket.on("data", (data) => socket.write(data)));
    const echoPort = await listen(echoServer, "127.0.0.1");

    const relay = await createProxyServer({
      host: "127.0.0.1",
      port: 0,
      authToken: "test-token",
      allowPrivateIps: true,
      metricsIntervalMs: 0,
    });

    // `createProxyServer` returns `ws://.../tcp-mux`; the browser client wants an
    // HTTP base URL and appends `/tcp-mux` itself.
    const relayWsUrl = new URL(relay.url);
    relayWsUrl.protocol = "http:";
    relayWsUrl.pathname = "/";
    relayWsUrl.search = "";

    const client = new WebSocketTcpMuxProxyClient(relayWsUrl.toString(), { authToken: "test-token" });
    const streamId = 1;

    const opened = new Promise<void>((resolve) => {
      client.onOpen = (id) => {
        if (id === streamId) resolve();
      };
    });

    let received = "";
    const gotHello = new Promise<void>((resolve, reject) => {
      client.onData = (id, data) => {
        if (id !== streamId) return;
        received += new TextDecoder().decode(data);
        if (received.includes("hello")) resolve();
      };
      client.onError = (id, err) => {
        if (id !== streamId) return;
        reject(new Error(`unexpected ERROR code=${err.code} message=${err.message}`));
      };
    });

    const closed = new Promise<void>((resolve) => {
      client.onClose = (id) => {
        if (id === streamId) resolve();
      };
    });

    const echoConnPromise = once(echoServer, "connection") as Promise<[net.Socket]>;

    client.open(streamId, "127.0.0.1", echoPort);
    client.send(streamId, new TextEncoder().encode("hello"));

    await withTimeout(opened, 2_000, "expected onOpen");
    await withTimeout(gotHello, 2_000, "expected DATA roundtrip");
    assert.ok(received.includes("hello"));

    const [echoSocket] = await withTimeout(echoConnPromise, 2_000, "expected echo TCP connection");
    const tcpEnd = once(echoSocket, "end");

    client.close(streamId, { fin: true });

    await withTimeout(tcpEnd, 2_000, "expected TCP FIN to reach echo server");
    await withTimeout(closed, 2_000, "expected onClose after FIN exchange");

    try {
      await client.shutdown();
    } finally {
      await relay.close();
      await closeServer(echoServer);
    }
  });
});
