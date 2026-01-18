import test from "node:test";
import assert from "node:assert/strict";
import net from "node:net";
import dgram from "node:dgram";
import { WebSocket } from "ws";
import { startProxyServer } from "../server";
import { decodeUdpRelayFrame, encodeUdpRelayV1Datagram } from "../udpRelayProtocol";
import {
  TCP_MUX_SUBPROTOCOL,
  TcpMuxFrameParser,
  TcpMuxMsgType,
  encodeTcpMuxFrame,
  encodeTcpMuxOpenPayload,
  type TcpMuxFrame
} from "../tcpMuxProtocol";

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

async function startUdpEchoServer(): Promise<{ port: number; close: () => Promise<void> }> {
  const server = dgram.createSocket("udp4");
  server.on("message", (msg, rinfo) => {
    server.send(msg, rinfo.port, rinfo.address);
  });

  await new Promise<void>((resolve, reject) => {
    server.once("error", reject);
    server.bind(0, "127.0.0.1", () => resolve());
  });
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

async function openWebSocket(url: string, protocol?: string | string[]): Promise<WebSocket> {
  const ws = protocol ? new WebSocket(url, protocol) : new WebSocket(url);
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

type FrameWaiter = {
  waitFor: (predicate: (frame: TcpMuxFrame) => boolean, timeoutMs?: number) => Promise<TcpMuxFrame>;
};

function createTcpMuxFrameWaiter(ws: WebSocket): FrameWaiter {
  const parser = new TcpMuxFrameParser();
  const backlog: TcpMuxFrame[] = [];
  const waiters: Array<{
    predicate: (frame: TcpMuxFrame) => boolean;
    resolve: (frame: TcpMuxFrame) => void;
    reject: (err: Error) => void;
    timer: NodeJS.Timeout;
  }> = [];

  ws.on("message", (data, isBinary) => {
    assert.equal(isBinary, true);
    const buf = Buffer.isBuffer(data)
      ? data
      : Array.isArray(data)
        ? Buffer.concat(data)
        : Buffer.from(data as ArrayBuffer);
    const frames = parser.push(buf);
    for (const frame of frames) {
      const waiterIdx = waiters.findIndex((w) => w.predicate(frame));
      if (waiterIdx !== -1) {
        const [w] = waiters.splice(waiterIdx, 1);
        clearTimeout(w!.timer);
        w!.resolve(frame);
        continue;
      }
      backlog.push(frame);
    }
  });

  const waitFor = (predicate: (frame: TcpMuxFrame) => boolean, timeoutMs = 2_000): Promise<TcpMuxFrame> => {
    const idx = backlog.findIndex(predicate);
    if (idx !== -1) {
      return Promise.resolve(backlog.splice(idx, 1)[0]!);
    }

    return new Promise<TcpMuxFrame>((resolve, reject) => {
      let waiter: (typeof waiters)[number];
      const timer = setTimeout(() => {
        const i = waiters.indexOf(waiter);
        if (i !== -1) waiters.splice(i, 1);
        reject(new Error("timeout waiting for frame"));
      }, timeoutMs);
      timer.unref();

      waiter = { predicate, resolve, reject, timer };
      waiters.push(waiter);
    });
  };

  return { waitFor };
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

test("proxy /metrics counts multiplexed udp bindings and bytes", async () => {
  const udpServer = await startUdpEchoServer();
  const proxy = await startProxyServer({ listenHost: "127.0.0.1", listenPort: 0, open: true });
  const addr = proxy.server.address();
  assert.ok(addr && typeof addr !== "string");

  try {
    const origin = `http://127.0.0.1:${addr.port}`;
    const { body: metrics0Body } = await fetchText(`${origin}/metrics`);
    const bindings0 = parseMetric(metrics0Body, "net_proxy_udp_bindings_active");
    const bytesIn0 = parseMetric(metrics0Body, "net_proxy_bytes_in_total", { proto: "udp" });
    const bytesOut0 = parseMetric(metrics0Body, "net_proxy_bytes_out_total", { proto: "udp" });
    const conns0 = parseMetric(metrics0Body, "net_proxy_connections_active", { proto: "udp" });
    assert.notEqual(bindings0, null, "missing udp bindings metric");
    assert.notEqual(bytesIn0, null, "missing udp bytes_in metric");
    assert.notEqual(bytesOut0, null, "missing udp bytes_out metric");
    assert.notEqual(conns0, null, "missing udp connections_active metric");

    const ws = await openWebSocket(`ws://127.0.0.1:${addr.port}/udp`);

    const guestPort = 54321;
    const payload = Buffer.from([9, 8, 7, 6]);
    const frame = encodeUdpRelayV1Datagram({
      guestPort,
      remoteIpv4: [127, 0, 0, 1],
      remotePort: udpServer.port,
      payload
    });

    const receivedPromise = waitForBinaryMessage(ws);
    ws.send(frame);
    const received = await receivedPromise;
    const decoded = decodeUdpRelayFrame(received);
    assert.equal(decoded.version, 1);
    assert.equal(decoded.guestPort, guestPort);
    assert.equal(decoded.remotePort, udpServer.port);
    assert.deepEqual(decoded.payload, payload);

    const deadline = Date.now() + 2_000;
    let bindings: bigint | null = null;
    let bytesIn: bigint | null = null;
    let bytesOut: bigint | null = null;
    let conns: bigint | null = null;
    while (Date.now() < deadline) {
      const { body } = await fetchText(`${origin}/metrics`);
      bindings = parseMetric(body, "net_proxy_udp_bindings_active");
      bytesIn = parseMetric(body, "net_proxy_bytes_in_total", { proto: "udp" });
      bytesOut = parseMetric(body, "net_proxy_bytes_out_total", { proto: "udp" });
      conns = parseMetric(body, "net_proxy_connections_active", { proto: "udp" });
      if (
        bindings !== null &&
        bytesIn !== null &&
        bytesOut !== null &&
        conns !== null &&
        bindings0 !== null &&
        bytesIn0 !== null &&
        bytesOut0 !== null &&
        conns0 !== null &&
        bindings >= bindings0 + 1n &&
        bytesIn >= bytesIn0 + BigInt(payload.length) &&
        bytesOut >= bytesOut0 + BigInt(payload.length) &&
        conns >= conns0 + 1n
      ) {
        break;
      }
      await sleep(25);
    }
    assert.ok(bindings0 !== null && bindings !== null && bindings >= bindings0 + 1n, "udp bindings did not increment");
    assert.ok(bytesIn0 !== null && bytesIn !== null && bytesIn >= bytesIn0 + BigInt(payload.length));
    assert.ok(bytesOut0 !== null && bytesOut !== null && bytesOut >= bytesOut0 + BigInt(payload.length));

    const closePromise = waitForClose(ws);
    ws.close(1000, "done");
    await closePromise;

    const closeDeadline = Date.now() + 2_000;
    let bindingsClosed: bigint | null = null;
    let connsClosed: bigint | null = null;
    while (Date.now() < closeDeadline) {
      const { body } = await fetchText(`${origin}/metrics`);
      bindingsClosed = parseMetric(body, "net_proxy_udp_bindings_active");
      connsClosed = parseMetric(body, "net_proxy_connections_active", { proto: "udp" });
      if (
        bindingsClosed !== null &&
        connsClosed !== null &&
        bindings0 !== null &&
        conns0 !== null &&
        bindingsClosed === bindings0 &&
        connsClosed === conns0
      ) {
        break;
      }
      await sleep(25);
    }
    assert.equal(bindingsClosed, bindings0);
    assert.equal(connsClosed, conns0);
  } finally {
    await proxy.close();
    await udpServer.close();
  }
});

test("proxy /metrics counts denied connection attempts", async () => {
  const echoServer = await startTcpEchoServer();
  const proxy = await startProxyServer({ listenHost: "127.0.0.1", listenPort: 0, open: false, allow: "" });
  const addr = proxy.server.address();
  assert.ok(addr && typeof addr !== "string");

  try {
    const origin = `http://127.0.0.1:${addr.port}`;
    const { body: metrics0Body } = await fetchText(`${origin}/metrics`);
    const denied0 = parseMetric(metrics0Body, "net_proxy_connection_errors_total", { kind: "denied" });
    assert.notEqual(denied0, null, "missing denied counter");

    const ws = new WebSocket(`ws://127.0.0.1:${addr.port}/tcp?v=1&host=127.0.0.1&port=${echoServer.port}`);
    const closed = await waitForClose(ws);
    assert.equal(closed.code, 1008);

    const deadline = Date.now() + 2_000;
    let denied: bigint | null = null;
    while (Date.now() < deadline) {
      const { body } = await fetchText(`${origin}/metrics`);
      denied = parseMetric(body, "net_proxy_connection_errors_total", { kind: "denied" });
      if (denied !== null && denied0 !== null && denied >= denied0 + 1n) break;
      await sleep(25);
    }
    assert.ok(denied0 !== null && denied !== null && denied >= denied0 + 1n, "denied counter did not increment");
  } finally {
    await proxy.close();
    await echoServer.close();
  }
});

test("proxy /metrics counts connection failures (tcp)", async () => {
  const temp = await startTcpEchoServer();
  const unusedPort = temp.port;
  await temp.close();

  const proxy = await startProxyServer({ listenHost: "127.0.0.1", listenPort: 0, open: true, connectTimeoutMs: 250 });
  const addr = proxy.server.address();
  assert.ok(addr && typeof addr !== "string");

  try {
    const origin = `http://127.0.0.1:${addr.port}`;
    const { body: metrics0Body } = await fetchText(`${origin}/metrics`);
    const err0 = parseMetric(metrics0Body, "net_proxy_connection_errors_total", { kind: "error" });
    assert.notEqual(err0, null, "missing error counter");

    const ws = new WebSocket(`ws://127.0.0.1:${addr.port}/tcp?v=1&host=127.0.0.1&port=${unusedPort}`);
    const closed = await waitForClose(ws);
    assert.equal(closed.code, 1011);

    const deadline = Date.now() + 2_000;
    let err: bigint | null = null;
    while (Date.now() < deadline) {
      const { body } = await fetchText(`${origin}/metrics`);
      err = parseMetric(body, "net_proxy_connection_errors_total", { kind: "error" });
      if (err !== null && err0 !== null && err >= err0 + 1n) break;
      await sleep(25);
    }
    assert.ok(err0 !== null && err !== null && err >= err0 + 1n, "error counter did not increment");
  } finally {
    await proxy.close();
  }
});

test("proxy /metrics does not leak active tcp connections when createTcpConnection throws", async () => {
  const proxy = await startProxyServer({
    listenHost: "127.0.0.1",
    listenPort: 0,
    open: true,
    createTcpConnection: () => {
      throw new Error("boom");
    }
  });
  const addr = proxy.server.address();
  assert.ok(addr && typeof addr !== "string");

  try {
    const origin = `http://127.0.0.1:${addr.port}`;
    const { body: metrics0Body } = await fetchText(`${origin}/metrics`);
    const err0 = parseMetric(metrics0Body, "net_proxy_connection_errors_total", { kind: "error" });
    const conns0 = parseMetric(metrics0Body, "net_proxy_connections_active", { proto: "tcp" });
    assert.notEqual(err0, null, "missing error counter");
    assert.notEqual(conns0, null, "missing tcp connections gauge");

    const ws = await openWebSocket(`ws://127.0.0.1:${addr.port}/tcp?v=1&host=127.0.0.1&port=80`);
    const closed = await waitForClose(ws);
    assert.equal(closed.code, 1011);

    const deadline = Date.now() + 2_000;
    let err: bigint | null = null;
    let conns: bigint | null = null;
    while (Date.now() < deadline) {
      const { body } = await fetchText(`${origin}/metrics`);
      err = parseMetric(body, "net_proxy_connection_errors_total", { kind: "error" });
      conns = parseMetric(body, "net_proxy_connections_active", { proto: "tcp" });
      if (err !== null && err0 !== null && err >= err0 + 1n && conns !== null && conns0 !== null && conns === conns0) {
        break;
      }
      await sleep(25);
    }
    assert.ok(err0 !== null && err !== null && err >= err0 + 1n, "error counter did not increment");
    assert.equal(conns, conns0);
  } finally {
    await proxy.close();
  }
});

test("proxy /metrics counts tcp-mux streams and bytes", async () => {
  const echoServer = await startTcpEchoServer();
  const proxy = await startProxyServer({ listenHost: "127.0.0.1", listenPort: 0, open: true });
  const addr = proxy.server.address();
  assert.ok(addr && typeof addr !== "string");

  let ws: WebSocket | null = null;
  try {
    const origin = `http://127.0.0.1:${addr.port}`;
    const { body: metrics0Body } = await fetchText(`${origin}/metrics`);
    const streams0 = parseMetric(metrics0Body, "net_proxy_tcp_connections_active", { proto: "tcp_mux" });
    const bytesIn0 = parseMetric(metrics0Body, "net_proxy_bytes_in_total", { proto: "tcp_mux" });
    const bytesOut0 = parseMetric(metrics0Body, "net_proxy_bytes_out_total", { proto: "tcp_mux" });
    const connsWs0 = parseMetric(metrics0Body, "net_proxy_connections_active", { proto: "tcp_mux" });
    assert.notEqual(streams0, null, "missing tcp-mux stream gauge");
    assert.notEqual(bytesIn0, null, "missing tcp-mux bytes_in counter");
    assert.notEqual(bytesOut0, null, "missing tcp-mux bytes_out counter");
    assert.notEqual(connsWs0, null, "missing tcp-mux ws gauge");

    ws = await openWebSocket(`ws://127.0.0.1:${addr.port}/tcp-mux`, TCP_MUX_SUBPROTOCOL);
    assert.equal(ws.protocol, TCP_MUX_SUBPROTOCOL);
    const waiter = createTcpMuxFrameWaiter(ws);

    const streamId = 1;
    const payload = Buffer.from("hello tcp-mux");

    ws.send(encodeTcpMuxFrame(TcpMuxMsgType.OPEN, streamId, encodeTcpMuxOpenPayload({ host: "127.0.0.1", port: echoServer.port })));
    ws.send(encodeTcpMuxFrame(TcpMuxMsgType.DATA, streamId, payload));

    // Wait for the echo before checking metrics so we know the stream is active.
    const chunks: Buffer[] = [];
    let total = 0;
    while (total < payload.length) {
      const frame = await waiter.waitFor((f) => f.msgType === TcpMuxMsgType.DATA && f.streamId === streamId);
      chunks.push(frame.payload);
      total += frame.payload.length;
    }
    assert.deepEqual(Buffer.concat(chunks), payload);

    // Metrics should reflect an active stream and bytes in/out.
    const deadline = Date.now() + 2_000;
    let streams: bigint | null = null;
    let bytesIn: bigint | null = null;
    let bytesOut: bigint | null = null;
    let connsWs: bigint | null = null;
    while (Date.now() < deadline) {
      const { body } = await fetchText(`${origin}/metrics`);
      streams = parseMetric(body, "net_proxy_tcp_connections_active", { proto: "tcp_mux" });
      bytesIn = parseMetric(body, "net_proxy_bytes_in_total", { proto: "tcp_mux" });
      bytesOut = parseMetric(body, "net_proxy_bytes_out_total", { proto: "tcp_mux" });
      connsWs = parseMetric(body, "net_proxy_connections_active", { proto: "tcp_mux" });
      if (
        streams !== null &&
        bytesIn !== null &&
        bytesOut !== null &&
        connsWs !== null &&
        streams0 !== null &&
        bytesIn0 !== null &&
        bytesOut0 !== null &&
        connsWs0 !== null &&
        streams >= streams0 + 1n &&
        bytesIn >= bytesIn0 + BigInt(payload.length) &&
        bytesOut >= bytesOut0 + BigInt(payload.length) &&
        connsWs >= connsWs0 + 1n
      ) {
        break;
      }
      await sleep(25);
    }

    assert.ok(streams0 !== null && streams !== null && streams >= streams0 + 1n, "tcp-mux stream gauge did not increment");
    assert.ok(bytesIn0 !== null && bytesIn !== null && bytesIn >= bytesIn0 + BigInt(payload.length));
    assert.ok(bytesOut0 !== null && bytesOut !== null && bytesOut >= bytesOut0 + BigInt(payload.length));

    const closePromise = waitForClose(ws);
    ws.close(1000, "done");
    await closePromise;
    ws = null;

    const closeDeadline = Date.now() + 2_000;
    let streamsClosed: bigint | null = null;
    let connsWsClosed: bigint | null = null;
    while (Date.now() < closeDeadline) {
      const { body } = await fetchText(`${origin}/metrics`);
      streamsClosed = parseMetric(body, "net_proxy_tcp_connections_active", { proto: "tcp_mux" });
      connsWsClosed = parseMetric(body, "net_proxy_connections_active", { proto: "tcp_mux" });
      if (streamsClosed !== null && connsWsClosed !== null && streams0 !== null && connsWs0 !== null && streamsClosed === streams0 && connsWsClosed === connsWs0) {
        break;
      }
      await sleep(25);
    }
    assert.equal(streamsClosed, streams0);
    assert.equal(connsWsClosed, connsWs0);
  } finally {
    ws?.terminate();
    await proxy.close();
    await echoServer.close();
  }
});
