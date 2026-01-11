import test from "node:test";
import assert from "node:assert/strict";

import { WebSocket } from "../tools/minimal_ws.js";
import { encodeL2Frame } from "../web/src/shared/l2TunnelProtocol.ts";

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
      reject(new Error("timeout waiting for websocket close"));
    }, timeoutMs);
    timeout.unref();

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
      cleanup();
      reject(err);
    };

    ws.on("close", onClose);
    ws.on("error", onError);
  });
}

test("l2 proxy exposes /metrics and counts rx frames", { timeout: 660_000 }, async () => {
  const proxy = await startRustL2Proxy({
    AERO_L2_OPEN: "1",
    AERO_L2_ALLOWED_ORIGINS: "",
    AERO_L2_TOKEN: "",
    AERO_L2_MAX_CONNECTIONS: "0",
  });
  const port = proxy.port;

  try {
    {
      const { res } = await fetchText(`http://127.0.0.1:${port}/readyz`, { timeoutMs: 2_000 });
      assert.equal(res.status, 200);
    }

    {
      const { res, json: body } = await fetchJson(`http://127.0.0.1:${port}/version`, { timeoutMs: 2_000 });
      assert.equal(res.status, 200);
      assert.equal(typeof body.version, "string");
      assert.equal(typeof body.gitSha, "string");
      assert.equal(typeof body.builtAt, "string");
    }

    const ws = new WebSocket(`ws://127.0.0.1:${port}/l2`, ["aero-l2-tunnel-v1"]);
    await waitForOpen(ws);

    const payload = Buffer.alloc(60);
    await new Promise((resolve, reject) => {
      ws.send(encodeL2Frame(payload), resolve);
      ws.once("error", reject);
    });
    ws.close(1000, "done");
    await waitForClose(ws);

    const deadline = Date.now() + 5_000;
    let rx = null;
    let bytesRx = null;
    let sessionsTotal = null;
    let sessionsActive = null;
    while (Date.now() < deadline) {
      const { res, text: body } = await fetchText(`http://127.0.0.1:${port}/metrics`, { timeoutMs: 2_000 });
      assert.equal(res.status, 200);
      rx = parseMetric(body, "l2_frames_rx_total");
      bytesRx = parseMetric(body, "l2_bytes_rx_total");
      sessionsTotal = parseMetric(body, "l2_sessions_total");
      sessionsActive = parseMetric(body, "l2_sessions_active");
      if (
        rx !== null &&
        rx >= 1 &&
        bytesRx !== null &&
        bytesRx >= payload.length &&
        sessionsTotal !== null &&
        sessionsTotal >= 1 &&
        sessionsActive === 0
      ) {
        break;
      }
      await sleep(25);
    }
    assert.ok(rx !== null && rx >= 1, `expected rx >= 1, got ${rx}`);
    assert.ok(bytesRx !== null && bytesRx >= payload.length, `expected bytes_rx >= ${payload.length}, got ${bytesRx}`);
    assert.ok(sessionsTotal !== null && sessionsTotal >= 1, `expected sessions_total >= 1, got ${sessionsTotal}`);
    assert.equal(sessionsActive, 0, `expected sessions_active == 0, got ${sessionsActive}`);
  } finally {
    await proxy.close();
  }
});
