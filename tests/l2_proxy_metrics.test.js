import test from "node:test";
import assert from "node:assert/strict";

import { wsCloseSafe, wsSendSafe } from "../scripts/_shared/ws_safe.js";
import { WebSocket } from "../tools/minimal_ws.js";
import { encodeL2Frame, L2_TUNNEL_SUBPROTOCOL } from "../web/src/shared/l2TunnelProtocol.js";

import { startRustL2Proxy } from "../tools/rust_l2_proxy.js";

function sleep(ms) {
  return new Promise((resolve) => {
    const timeout = setTimeout(resolve, ms);
    timeout.unref();
  });
}

async function fetchText(url, { timeoutMs }) {
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), timeoutMs);
  timeout.unref();
  try {
    const res = await fetch(url, { signal: controller.signal });
    return { res, text: await res.text() };
  } finally {
    clearTimeout(timeout);
  }
}

async function fetchJson(url, { timeoutMs }) {
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), timeoutMs);
  timeout.unref();
  try {
    const res = await fetch(url, { signal: controller.signal });
    return { res, json: await res.json() };
  } finally {
    clearTimeout(timeout);
  }
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

async function waitForOpen(ws, timeoutMs = 2_000) {
  return new Promise((resolve, reject) => {
    const timeout = setTimeout(() => {
      cleanup();
      reject(new Error("timeout waiting for websocket open"));
    }, timeoutMs);
    timeout.unref();

    let settled = false;
    const cleanup = () => {
      if (settled) return;
      settled = true;
      clearTimeout(timeout);
      ws.off("open", onOpen);
      ws.off("error", onError);
      ws.off("close", onClose);
      ws.off("unexpected-response", onUnexpectedResponse);
    };

    const onOpen = () => {
      cleanup();
      resolve();
    };

    const onError = (err) => {
      cleanup();
      reject(err);
    };

    const onClose = (code, reason) => {
      cleanup();
      reject(
        new Error(
          `websocket closed before open (code=${code}, reason=${reason.toString("utf8")})`,
        ),
      );
    };

    const onUnexpectedResponse = (_req, res) => {
      const chunks = [];
      res.on("data", (c) => chunks.push(c));
      res.on("end", () => {
        cleanup();
        reject(
          new Error(
            `unexpected websocket response (${res.statusCode ?? 0}): ${Buffer.concat(chunks).toString("utf8")}`,
          ),
        );
      });
      res.on("error", onError);
    };

    ws.on("open", onOpen);
    ws.on("error", onError);
    ws.on("close", onClose);
    ws.on("unexpected-response", onUnexpectedResponse);
  });
}

async function waitForClose(ws, timeoutMs = 2_000) {
  return new Promise((resolve, reject) => {
    const timeout = setTimeout(() => {
      cleanup();
      const suffix = lastErr ? ` (last error: ${lastErr instanceof Error ? lastErr.message : String(lastErr)})` : "";
      reject(new Error(`timeout waiting for websocket close${suffix}`));
    }, timeoutMs);
    timeout.unref();

    let lastErr = null;
    let settled = false;
    const cleanup = () => {
      if (settled) return;
      settled = true;
      clearTimeout(timeout);
      ws.off("close", onClose);
      ws.off("error", onError);
    };

    const onClose = (code, reason) => {
      cleanup();
      resolve({ code, reason: reason.toString("utf8") });
    };

    const onError = (err) => {
      lastErr = err;
      // Some servers reset the TCP socket immediately after sending a WebSocket close frame.
      // The client may observe `ECONNRESET` while attempting to write the close response.
      // Treat this as non-fatal as long as we still receive a `close` event.
    };

    ws.on("close", onClose);
    ws.on("error", onError);
  });
}

test("l2 proxy exposes /metrics and counts rx frames", { timeout: 900_000 }, async () => {
  const proxy = await startRustL2Proxy({
    AERO_L2_OPEN: "1",
    AERO_L2_ALLOWED_ORIGINS: "",
    AERO_L2_AUTH_MODE: "none",
    AERO_L2_TOKEN: "",
    AERO_L2_MAX_CONNECTIONS: "0",
  });
  const port = proxy.port;

  try {
    const origin = `http://127.0.0.1:${port}`;

    {
      const { res } = await fetchText(`${origin}/readyz`, { timeoutMs: 2_000 });
      assert.equal(res.status, 200);
    }

    {
      const { res, json: body } = await fetchJson(`${origin}/version`, { timeoutMs: 2_000 });
      assert.equal(res.status, 200);
      assert.equal(typeof body.version, "string");
      assert.equal(typeof body.gitSha, "string");
      assert.equal(typeof body.builtAt, "string");
    }

    // Metrics should be available even before any sessions.
    const { res: metrics0Res, text: metrics0Body } = await fetchText(`${origin}/metrics`, { timeoutMs: 2_000 });
    assert.equal(metrics0Res.status, 200);
    const rx0 = parseMetric(metrics0Body, "l2_frames_rx_total");
    const bytesRx0 = parseMetric(metrics0Body, "l2_bytes_rx_total");
    const sessionsTotal0 = parseMetric(metrics0Body, "l2_sessions_total");
    const sessionsActive0 = parseMetric(metrics0Body, "l2_sessions_active");
    assert.notEqual(rx0, null, "missing l2_frames_rx_total");
    assert.notEqual(bytesRx0, null, "missing l2_bytes_rx_total");
    assert.notEqual(sessionsTotal0, null, "missing l2_sessions_total");
    assert.notEqual(sessionsActive0, null, "missing l2_sessions_active");

    const ws = new WebSocket(`ws://127.0.0.1:${port}/l2`, [L2_TUNNEL_SUBPROTOCOL]);
    await waitForOpen(ws);

    const payload = Buffer.alloc(60);
    await new Promise((resolve, reject) => {
      wsSendSafe(ws, encodeL2Frame(payload), (err) => (err ? reject(err) : resolve()));
      ws.once("error", reject);
    });
    wsCloseSafe(ws, 1000, "done");
    await waitForClose(ws);

    const deadline = Date.now() + 5_000;
    let rx = null;
    let bytesRx = null;
    let sessionsTotal = null;
    let sessionsActive = null;
    while (Date.now() < deadline) {
      const { res, text: body } = await fetchText(`${origin}/metrics`, { timeoutMs: 2_000 });
      assert.equal(res.status, 200);
      rx = parseMetric(body, "l2_frames_rx_total");
      bytesRx = parseMetric(body, "l2_bytes_rx_total");
      sessionsTotal = parseMetric(body, "l2_sessions_total");
      sessionsActive = parseMetric(body, "l2_sessions_active");
      if (
        rx !== null &&
        rx >= rx0 + 1 &&
        bytesRx !== null &&
        bytesRx >= bytesRx0 + payload.length &&
        sessionsTotal !== null &&
        sessionsTotal >= sessionsTotal0 + 1 &&
        sessionsActive === 0
      ) {
        break;
      }
      await sleep(25);
    }
    assert.ok(rx !== null && rx >= rx0 + 1, `expected rx >= ${rx0 + 1}, got ${rx}`);
    assert.ok(
      bytesRx !== null && bytesRx >= bytesRx0 + payload.length,
      `expected bytes_rx >= ${bytesRx0 + payload.length}, got ${bytesRx}`,
    );
    assert.ok(
      sessionsTotal !== null && sessionsTotal >= sessionsTotal0 + 1,
      `expected sessions_total >= ${sessionsTotal0 + 1}, got ${sessionsTotal}`,
    );
    assert.equal(sessionsActive, 0, `expected sessions_active == 0, got ${sessionsActive}`);
  } finally {
    await proxy.close();
  }
});
