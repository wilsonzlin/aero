import test from "node:test";
import assert from "node:assert/strict";

import { WebSocket } from "../tools/minimal_ws.js";
import { encodeL2Frame } from "../web/src/shared/l2TunnelProtocol.ts";

import { startRustL2Proxy } from "../tools/rust_l2_proxy.js";

const L2_PROXY_TEST_TIMEOUT_MS = 660_000;

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
    let lastErr = null;
    const timeout = setTimeout(() => {
      const suffix = lastErr ? ` (last error: ${lastErr instanceof Error ? lastErr.message : String(lastErr)})` : "";
      reject(new Error(`timeout waiting for close${suffix}`));
    }, timeoutMs);
    timeout.unref();
    ws.once("close", (code, reason) => {
      clearTimeout(timeout);
      ws.off("error", onError);
      resolve({ code, reason: reason.toString() });
    });
    const onError = (err) => {
      lastErr = err;
      // Some servers reset the TCP socket immediately after sending a WebSocket close frame.
      // The client may observe `ECONNRESET` while attempting to write the close response.
      // Treat this as non-fatal as long as we still receive a `close` event.
    };
    ws.once("error", onError);
  });
}

test("l2 proxy requires Origin by default", { timeout: L2_PROXY_TEST_TIMEOUT_MS }, async () => {
  const proxy = await startRustL2Proxy({
    AERO_L2_OPEN: "0",
    AERO_L2_ALLOWED_ORIGINS: "https://app.example.com",
    AERO_L2_TOKEN: "",
    AERO_L2_MAX_CONNECTIONS: "0",
  });

  try {
    const denied = await connectOrReject(`ws://127.0.0.1:${proxy.port}/l2`);
    assert.equal(denied.ok, false);
    assert.equal(denied.status, 403);

    const allowed = await connectOrReject(`ws://127.0.0.1:${proxy.port}/l2`, {
      headers: { origin: "https://app.example.com" },
    });
    assert.equal(allowed.ok, true);
    allowed.ws.close(1000, "done");
    await waitForClose(allowed.ws);
  } finally {
    await proxy.close();
  }
});

test("l2 proxy enforces token auth when configured", { timeout: L2_PROXY_TEST_TIMEOUT_MS }, async () => {
  const proxy = await startRustL2Proxy({
    AERO_L2_OPEN: "0",
    AERO_L2_ALLOWED_ORIGINS: "https://app.example.com",
    AERO_L2_TOKEN: "sekrit",
    AERO_L2_MAX_CONNECTIONS: "0",
  });

  try {
    const missing = await connectOrReject(`ws://127.0.0.1:${proxy.port}/l2`, {
      headers: { origin: "https://app.example.com" },
    });
    assert.equal(missing.ok, false);
    assert.equal(missing.status, 401);

    const wrong = await connectOrReject(`ws://127.0.0.1:${proxy.port}/l2?token=nope`, {
      headers: { origin: "https://app.example.com" },
    });
    assert.equal(wrong.ok, false);
    assert.equal(wrong.status, 401);

    const ok = await connectOrReject(`ws://127.0.0.1:${proxy.port}/l2?token=sekrit`, {
      headers: { origin: "https://app.example.com" },
    });
    assert.equal(ok.ok, true);
    ok.ws.close(1000, "done");
    await waitForClose(ok.ws);

    const protoOk = await connectOrReject(`ws://127.0.0.1:${proxy.port}/l2`, {
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

test("AERO_L2_OPEN disables Origin enforcement (but not token auth)", { timeout: L2_PROXY_TEST_TIMEOUT_MS }, async () => {
  const proxy = await startRustL2Proxy({
    AERO_L2_OPEN: "1",
    AERO_L2_ALLOWED_ORIGINS: "",
    AERO_L2_TOKEN: "sekrit",
    AERO_L2_MAX_CONNECTIONS: "0",
  });

  try {
    const denied = await connectOrReject(`ws://127.0.0.1:${proxy.port}/l2`);
    assert.equal(denied.ok, false);
    assert.equal(denied.status, 401);

    const ok = await connectOrReject(`ws://127.0.0.1:${proxy.port}/l2?token=sekrit`);
    assert.equal(ok.ok, true);
    ok.ws.close(1000, "done");
    await waitForClose(ok.ws);
  } finally {
    await proxy.close();
  }
});

test("l2 proxy enforces max connection quota at upgrade time", { timeout: L2_PROXY_TEST_TIMEOUT_MS }, async () => {
  const proxy = await startRustL2Proxy({
    AERO_L2_OPEN: "1",
    AERO_L2_ALLOWED_ORIGINS: "",
    AERO_L2_TOKEN: "",
    AERO_L2_MAX_CONNECTIONS: "1",
  });

  try {
    const first = await connectOrReject(`ws://127.0.0.1:${proxy.port}/l2`);
    assert.equal(first.ok, true);

    const second = await connectOrReject(`ws://127.0.0.1:${proxy.port}/l2`);
    assert.equal(second.ok, false);
    assert.equal(second.status, 429);

    first.ws.close(1000, "done");
    await waitForClose(first.ws);
  } finally {
    await proxy.close();
  }
});

test("l2 proxy closes the socket when per-connection quotas are exceeded", { timeout: L2_PROXY_TEST_TIMEOUT_MS }, async () => {
  const proxy = await startRustL2Proxy({
    AERO_L2_OPEN: "1",
    AERO_L2_ALLOWED_ORIGINS: "",
    AERO_L2_TOKEN: "",
    AERO_L2_MAX_CONNECTIONS: "0",
    AERO_L2_MAX_BYTES_PER_CONNECTION: "64",
    AERO_L2_MAX_FRAMES_PER_SECOND: "2",
  });

  try {
    const conn = await connectOrReject(`ws://127.0.0.1:${proxy.port}/l2`);
    assert.equal(conn.ok, true);

    // Byte quota is enforced on total WebSocket bytes (rx + tx), so send a single oversized tunnel
    // message (wire header + payload) that exceeds the configured limit.
    conn.ws.send(encodeL2Frame(Buffer.alloc(61)));
    const closedBytes = await waitForClose(conn.ws);
    assert.equal(closedBytes.code, 1008);
    assert.match(closedBytes.reason, /byte quota exceeded/i);

    const conn2 = await connectOrReject(`ws://127.0.0.1:${proxy.port}/l2`);
    assert.equal(conn2.ok, true);
    // Use zero-length text frames to exercise the FPS limiter without consuming the byte quota.
    conn2.ws.send("");
    conn2.ws.send("");
    conn2.ws.send("");
    const closedFps = await waitForClose(conn2.ws);
    assert.equal(closedFps.code, 1008);
    assert.match(closedFps.reason, /frame rate quota exceeded/i);
  } finally {
    await proxy.close();
  }
});
