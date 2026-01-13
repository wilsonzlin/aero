import test from "node:test";
import assert from "node:assert/strict";
import net from "node:net";
import { WebSocket } from "ws";
import { startProxyServer } from "../server";

async function fetchText(url: string, timeoutMs = 2_000): Promise<{ status: number; contentType: string | null; body: string }> {
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), timeoutMs);
  timeout.unref();
  try {
    const res = await fetch(url, { signal: controller.signal });
    return { status: res.status, contentType: res.headers.get("content-type"), body: await res.text() };
  } finally {
    clearTimeout(timeout);
  }
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => {
    const timeout = setTimeout(resolve, ms);
    timeout.unref();
  });
}

function parseMetric(body: string, name: string, labels: Record<string, string> = {}): bigint | null {
  const labelEntries = Object.entries(labels);
  const labelPart =
    labelEntries.length === 0 ? "" : `{${labelEntries.map(([k, v]) => `${k}="${v}"`).join(",")}}`;
  const key = `${name}${labelPart}`;

  for (const line of body.split("\n")) {
    const trimmed = line.trim();
    if (!trimmed || trimmed.startsWith("#")) continue;
    const [k, v] = trimmed.split(/\s+/, 2);
    if (k !== key) continue;
    if (!v) throw new Error(`missing metric value for ${key}`);
    try {
      return BigInt(v);
    } catch {
      throw new Error(`invalid metric value for ${key}: ${v}`);
    }
  }
  return null;
}

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

test("proxy exposes /metrics with expected metric names", async () => {
  const proxy = await startProxyServer({ listenHost: "127.0.0.1", listenPort: 0, open: true });
  const addr = proxy.server.address();
  assert.ok(addr && typeof addr !== "string");

  try {
    const origin = `http://127.0.0.1:${addr.port}`;
    const { status, contentType, body } = await fetchText(`${origin}/metrics`);
    assert.equal(status, 200);
    assert.ok(contentType?.includes("text/plain"), `expected text/plain content-type, got ${contentType ?? "null"}`);

    for (const name of [
      "net_proxy_connections_active",
      "net_proxy_tcp_connections_active",
      "net_proxy_udp_bindings_active",
      "net_proxy_bytes_in_total",
      "net_proxy_bytes_out_total",
      "net_proxy_connection_errors_total"
    ]) {
      assert.ok(body.includes(name), `missing metric ${name}`);
    }
  } finally {
    await proxy.close();
  }
});

test("proxy /metrics counts tcp relay bytes and active connections", async () => {
  const echoServer = await startTcpEchoServer();
  const proxy = await startProxyServer({ listenHost: "127.0.0.1", listenPort: 0, open: true });
  const addr = proxy.server.address();
  assert.ok(addr && typeof addr !== "string");

  try {
    const origin = `http://127.0.0.1:${addr.port}`;
    const { status: metrics0Status, body: metrics0Body } = await fetchText(`${origin}/metrics`);
    assert.equal(metrics0Status, 200);

    const bytesIn0 = parseMetric(metrics0Body, "net_proxy_bytes_in_total", { proto: "tcp" });
    const bytesOut0 = parseMetric(metrics0Body, "net_proxy_bytes_out_total", { proto: "tcp" });
    const conns0 = parseMetric(metrics0Body, "net_proxy_connections_active", { proto: "tcp" });
    assert.notEqual(bytesIn0, null, "missing tcp bytes_in metric");
    assert.notEqual(bytesOut0, null, "missing tcp bytes_out metric");
    assert.notEqual(conns0, null, "missing tcp connections_active metric");

    const ws = await openWebSocket(`ws://127.0.0.1:${addr.port}/tcp?v=1&host=127.0.0.1&port=${echoServer.port}`);

    // Wait for metrics to observe the connection.
    const openDeadline = Date.now() + 2_000;
    let connsOpen: bigint | null = null;
    while (Date.now() < openDeadline) {
      const { body } = await fetchText(`${origin}/metrics`);
      connsOpen = parseMetric(body, "net_proxy_connections_active", { proto: "tcp" });
      if (connsOpen !== null && conns0 !== null && connsOpen >= conns0 + 1n) break;
      await sleep(25);
    }
    assert.ok(connsOpen !== null && conns0 !== null && connsOpen >= conns0 + 1n, "tcp connections_active did not increment");

    const payload = Buffer.from([0, 1, 2, 3, 4, 5, 255]);
    const receivedPromise = waitForBinaryMessage(ws);
    ws.send(payload);
    const received = await receivedPromise;
    assert.deepEqual(received, payload);

    const closePromise = waitForClose(ws);
    ws.close(1000, "done");
    await closePromise;

    const deadline = Date.now() + 2_000;
    let bytesIn: bigint | null = null;
    let bytesOut: bigint | null = null;
    let conns: bigint | null = null;
    while (Date.now() < deadline) {
      const { body } = await fetchText(`${origin}/metrics`);
      bytesIn = parseMetric(body, "net_proxy_bytes_in_total", { proto: "tcp" });
      bytesOut = parseMetric(body, "net_proxy_bytes_out_total", { proto: "tcp" });
      conns = parseMetric(body, "net_proxy_connections_active", { proto: "tcp" });
      if (
        bytesIn !== null &&
        bytesOut !== null &&
        conns !== null &&
        bytesIn0 !== null &&
        bytesOut0 !== null &&
        conns0 !== null &&
        bytesIn >= bytesIn0 + BigInt(payload.length) &&
        bytesOut >= bytesOut0 + BigInt(payload.length) &&
        conns === conns0
      ) {
        break;
      }
      await sleep(25);
    }

    assert.ok(bytesIn0 !== null && bytesIn !== null && bytesIn >= bytesIn0 + BigInt(payload.length));
    assert.ok(bytesOut0 !== null && bytesOut !== null && bytesOut >= bytesOut0 + BigInt(payload.length));
    assert.equal(conns, conns0);
  } finally {
    await proxy.close();
    await echoServer.close();
  }
});
