import test from "node:test";
import assert from "node:assert/strict";

import { WebSocket } from "ws";

import { startL2ProxyServer } from "../proxy/aero-l2-proxy/src/server.js";

function getServerPort(server) {
  const addr = server.address();
  assert.ok(addr && typeof addr !== "string");
  return addr.port;
}

async function connectOrReject(url, { protocols, ...opts } = {}) {
  return new Promise((resolve, reject) => {
    const protos = protocols ?? ["aero-l2-tunnel-v1"];
    const ws = new WebSocket(url, protos, opts);

    const timeout = setTimeout(() => reject(new Error("timeout waiting for websocket outcome")), 2_000);
    timeout.unref();

    let settled = false;
    const settle = (v) => {
      if (settled) return;
      settled = true;
      clearTimeout(timeout);
      resolve(v);
    };

    ws.once("open", () => settle({ ok: true, ws }));
    ws.once("unexpected-response", (_req, res) => {
      const chunks = [];
      res.on("data", (c) => chunks.push(c));
      res.on("end", () =>
        settle({
          ok: false,
          status: res.statusCode ?? 0,
          body: Buffer.concat(chunks).toString("utf8"),
        }),
      );
    });
    ws.once("error", (err) => {
      if (settled) return;
      clearTimeout(timeout);
      reject(err);
    });
  });
}

async function waitForClose(ws, timeoutMs = 2_000) {
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

test("l2 proxy requires Origin by default", async () => {
  const proxy = await startL2ProxyServer({
    listenHost: "127.0.0.1",
    listenPort: 0,
    open: false,
    allowedOrigins: ["https://app.example.com"],
    token: null,
    maxConnections: 0,
  });
  const port = getServerPort(proxy.server);

  try {
    const denied = await connectOrReject(`ws://127.0.0.1:${port}/l2`);
    assert.equal(denied.ok, false);
    assert.equal(denied.status, 403);

    const allowed = await connectOrReject(`ws://127.0.0.1:${port}/l2`, {
      headers: { origin: "https://app.example.com" },
    });
    assert.equal(allowed.ok, true);
    allowed.ws.close(1000, "done");
    await waitForClose(allowed.ws);
  } finally {
    await proxy.close();
  }
});

test("l2 proxy enforces token auth when configured", async () => {
  const proxy = await startL2ProxyServer({
    listenHost: "127.0.0.1",
    listenPort: 0,
    open: false,
    allowedOrigins: ["https://app.example.com"],
    token: "sekrit",
    maxConnections: 0,
  });
  const port = getServerPort(proxy.server);

  try {
    const missing = await connectOrReject(`ws://127.0.0.1:${port}/l2`, {
      headers: { origin: "https://app.example.com" },
    });
    assert.equal(missing.ok, false);
    assert.equal(missing.status, 401);

    const wrong = await connectOrReject(`ws://127.0.0.1:${port}/l2?token=nope`, {
      headers: { origin: "https://app.example.com" },
    });
    assert.equal(wrong.ok, false);
    assert.equal(wrong.status, 401);

    const ok = await connectOrReject(`ws://127.0.0.1:${port}/l2?token=sekrit`, {
      headers: { origin: "https://app.example.com" },
    });
    assert.equal(ok.ok, true);
    ok.ws.close(1000, "done");
    await waitForClose(ok.ws);

    const protoOk = await connectOrReject(`ws://127.0.0.1:${port}/l2`, {
      protocols: ["aero-l2-tunnel-v1", "aero-l2-token.sekrit"],
      headers: { origin: "https://app.example.com" },
    });
    assert.equal(protoOk.ok, true);
    protoOk.ws.close(1000, "done");
    await waitForClose(protoOk.ws);
  } finally {
    await proxy.close();
  }
});

test("AERO_L2_OPEN disables Origin enforcement (but not token auth)", async () => {
  const proxy = await startL2ProxyServer({
    listenHost: "127.0.0.1",
    listenPort: 0,
    open: true,
    allowedOrigins: [],
    token: "sekrit",
    maxConnections: 0,
  });
  const port = getServerPort(proxy.server);

  try {
    const denied = await connectOrReject(`ws://127.0.0.1:${port}/l2`);
    assert.equal(denied.ok, false);
    assert.equal(denied.status, 401);

    const ok = await connectOrReject(`ws://127.0.0.1:${port}/l2?token=sekrit`);
    assert.equal(ok.ok, true);
    ok.ws.close(1000, "done");
    await waitForClose(ok.ws);
  } finally {
    await proxy.close();
  }
});

test("l2 proxy enforces max connection quota at upgrade time", async () => {
  const proxy = await startL2ProxyServer({
    listenHost: "127.0.0.1",
    listenPort: 0,
    open: true,
    allowedOrigins: [],
    token: null,
    maxConnections: 1,
  });
  const port = getServerPort(proxy.server);

  try {
    const first = await connectOrReject(`ws://127.0.0.1:${port}/l2`);
    assert.equal(first.ok, true);

    const second = await connectOrReject(`ws://127.0.0.1:${port}/l2`);
    assert.equal(second.ok, false);
    assert.equal(second.status, 429);

    first.ws.close(1000, "done");
    await waitForClose(first.ws);
  } finally {
    await proxy.close();
  }
});

test("l2 proxy closes the socket when per-connection quotas are exceeded", async () => {
  const proxy = await startL2ProxyServer({
    listenHost: "127.0.0.1",
    listenPort: 0,
    open: true,
    allowedOrigins: [],
    token: null,
    maxConnections: 0,
    maxBytesPerConnection: 10,
    maxFramesPerSecond: 2,
  });
  const port = getServerPort(proxy.server);

  try {
    const conn = await connectOrReject(`ws://127.0.0.1:${port}/l2`);
    assert.equal(conn.ok, true);

    conn.ws.send(Buffer.alloc(11));
    const closedBytes = await waitForClose(conn.ws);
    assert.equal(closedBytes.code, 1008);
    assert.match(closedBytes.reason, /Byte quota exceeded/);

    const conn2 = await connectOrReject(`ws://127.0.0.1:${port}/l2`);
    assert.equal(conn2.ok, true);
    conn2.ws.send(Buffer.from([1]));
    conn2.ws.send(Buffer.from([2]));
    conn2.ws.send(Buffer.from([3]));
    const closedFps = await waitForClose(conn2.ws);
    assert.equal(closedFps.code, 1008);
    assert.match(closedFps.reason, /Frame rate limit exceeded/);
  } finally {
    await proxy.close();
  }
});
