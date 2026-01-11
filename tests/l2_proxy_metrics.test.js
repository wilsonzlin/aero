import test from "node:test";
import assert from "node:assert/strict";

import { WebSocket } from "../tools/minimal_ws.js";

import { startL2ProxyServer } from "../proxy/aero-l2-proxy/src/server.ts";

function getServerPort(server) {
  const addr = server.address();
  assert.ok(addr && typeof addr !== "string");
  return addr.port;
}

function parseMetric(body, name) {
  for (const line of body.split("\n")) {
    const trimmed = line.trim();
    if (!trimmed || trimmed.startsWith("#")) continue;
    const [k, v] = trimmed.split(" ", 2);
    if (k === name) {
      const n = Number(v);
      if (!Number.isFinite(n)) throw new Error(`invalid metric value for ${name}: ${v}`);
      return n;
    }
  }
  return null;
}

async function waitForOpen(ws) {
  return new Promise((resolve, reject) => {
    ws.once("open", resolve);
    ws.once("error", reject);
  });
}

async function waitForClose(ws) {
  return new Promise((resolve, reject) => {
    ws.once("close", (code, reason) => resolve({ code, reason: reason.toString("utf8") }));
    ws.once("error", reject);
  });
}

test("node l2 proxy exposes /metrics and counts rx frames", async () => {
  const proxy = await startL2ProxyServer({
    listenHost: "127.0.0.1",
    listenPort: 0,
    open: true,
    allowedOrigins: [],
    token: null,
    maxConnections: 0,
  });
  const port = getServerPort(proxy.server);

  try {
    const ws = new WebSocket(`ws://127.0.0.1:${port}/l2`, ["aero-l2-tunnel-v1"]);
    await waitForOpen(ws);
    ws.send(Buffer.alloc(60));
    ws.close(1000, "done");
    await waitForClose(ws);

    const deadline = Date.now() + 2_000;
    let rx = null;
    let sessionsTotal = null;
    let sessionsActive = null;
    while (Date.now() < deadline) {
      const res = await fetch(`http://127.0.0.1:${port}/metrics`);
      assert.equal(res.status, 200);
      const body = await res.text();
      rx = parseMetric(body, "l2_frames_rx_total");
      sessionsTotal = parseMetric(body, "l2_sessions_total");
      sessionsActive = parseMetric(body, "l2_sessions_active");
      if (rx !== null && rx >= 1 && sessionsTotal !== null && sessionsTotal >= 1 && sessionsActive === 0) break;
      await new Promise((resolve) => setTimeout(resolve, 10));
    }
    assert.ok(rx !== null && rx >= 1, `expected rx >= 1, got ${rx}`);
    assert.ok(sessionsTotal !== null && sessionsTotal >= 1, `expected sessions_total >= 1, got ${sessionsTotal}`);
    assert.equal(sessionsActive, 0, `expected sessions_active == 0, got ${sessionsActive}`);
  } finally {
    await proxy.close();
  }
});
