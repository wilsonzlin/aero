import test from "node:test";
import assert from "node:assert/strict";
import { createHmac } from "node:crypto";

import { WebSocket } from "../tools/minimal_ws.js";
import { encodeL2Frame, L2_TUNNEL_SUBPROTOCOL, L2_TUNNEL_TOKEN_SUBPROTOCOL_PREFIX } from "../web/src/shared/l2TunnelProtocol.js";

import { startRustL2Proxy } from "../tools/rust_l2_proxy.js";

const L2_PROXY_TEST_TIMEOUT_MS = 900_000;

function sleep(ms) {
  return new Promise((resolve) => {
    const timeout = setTimeout(resolve, ms);
    timeout.unref();
  });
}

function base64UrlNoPad(buf) {
  return buf
    .toString("base64")
    .replace(/\+/g, "-")
    .replace(/\//g, "_")
    .replace(/=+$/g, "");
}

function makeSessionCookie({ secret, sid, expUnixSeconds }) {
  const payload = Buffer.from(JSON.stringify({ v: 1, sid, exp: expUnixSeconds }), "utf8");
  const payloadB64 = base64UrlNoPad(payload);
  const sig = createHmac("sha256", secret).update(payloadB64).digest();
  const sigB64 = base64UrlNoPad(sig);
  return `aero_session=${payloadB64}.${sigB64}`;
}

function makeJwtHs256({ secret, claims }) {
  const header = { alg: "HS256", typ: "JWT" };
  const headerB64 = base64UrlNoPad(Buffer.from(JSON.stringify(header), "utf8"));
  const payloadB64 = base64UrlNoPad(Buffer.from(JSON.stringify(claims), "utf8"));
  const signingInput = `${headerB64}.${payloadB64}`;
  const sig = createHmac("sha256", secret).update(signingInput).digest();
  const sigB64 = base64UrlNoPad(sig);
  return `${signingInput}.${sigB64}`;
}

async function connectOrReject(url, { protocols, ...opts } = {}) {
  return new Promise((resolve, reject) => {
    const protos = protocols ?? [L2_TUNNEL_SUBPROTOCOL];
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

test("l2 proxy requires Sec-WebSocket-Protocol: aero-l2-tunnel-v1 (for /l2 and /eth)", { timeout: L2_PROXY_TEST_TIMEOUT_MS }, async () => {
  const proxy = await startRustL2Proxy({
    AERO_L2_OPEN: "1",
    AERO_L2_ALLOWED_ORIGINS: "",
    AERO_L2_AUTH_MODE: "none",
    AERO_L2_TOKEN: "",
    AERO_L2_MAX_CONNECTIONS: "0",
  });

  try {
    for (const path of ["/l2", "/l2/", "/eth", "/eth/"]) {
      const res = await connectOrReject(`ws://127.0.0.1:${proxy.port}${path}`, { protocols: [] });
      assert.equal(res.ok, false);
      assert.equal(res.status, 400);
    }
  } finally {
    await proxy.close();
  }
});

test("l2 proxy requires Origin by default", { timeout: L2_PROXY_TEST_TIMEOUT_MS }, async () => {
  const proxy = await startRustL2Proxy({
    AERO_L2_OPEN: "0",
    AERO_L2_ALLOWED_ORIGINS: "https://app.example.com",
    AERO_L2_AUTH_MODE: "none",
    AERO_L2_TOKEN: "",
    AERO_L2_MAX_CONNECTIONS: "0",
  });

  try {
    const denied = await connectOrReject(`ws://127.0.0.1:${proxy.port}/l2`);
    assert.equal(denied.ok, false);
    assert.equal(denied.status, 403);

    const disallowed = await connectOrReject(`ws://127.0.0.1:${proxy.port}/l2`, {
      headers: { origin: "https://evil.example.com" },
    });
    assert.equal(disallowed.ok, false);
    assert.equal(disallowed.status, 403);

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

test("l2 proxy supports ALLOWED_ORIGINS fallback", { timeout: L2_PROXY_TEST_TIMEOUT_MS }, async () => {
  const proxy = await startRustL2Proxy({
    AERO_L2_OPEN: "0",
    // Ensure fallback is used even if callers pass through an empty env var.
    AERO_L2_ALLOWED_ORIGINS: "",
    ALLOWED_ORIGINS: "https://app.example.com",
    AERO_L2_AUTH_MODE: "none",
    AERO_L2_TOKEN: "",
    AERO_L2_MAX_CONNECTIONS: "0",
  });

  try {
    const denied = await connectOrReject(`ws://127.0.0.1:${proxy.port}/l2`);
    assert.equal(denied.ok, false);
    assert.equal(denied.status, 403);

    const disallowed = await connectOrReject(`ws://127.0.0.1:${proxy.port}/l2`, {
      headers: { origin: "https://evil.example.com" },
    });
    assert.equal(disallowed.ok, false);
    assert.equal(disallowed.status, 403);

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
    AERO_L2_AUTH_MODE: "token",
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
      protocols: [L2_TUNNEL_SUBPROTOCOL, `${L2_TUNNEL_TOKEN_SUBPROTOCOL_PREFIX}sekrit`],
      headers: { origin: "https://app.example.com" },
    });
    assert.equal(protoOk.ok, true);
    protoOk.ws.close(1000, "done");
    await waitForClose(protoOk.ws);
  } finally {
    await proxy.close();
  }
});

test("token errors take precedence over Origin errors", { timeout: L2_PROXY_TEST_TIMEOUT_MS }, async () => {
  const proxy = await startRustL2Proxy({
    AERO_L2_OPEN: "0",
    AERO_L2_ALLOWED_ORIGINS: "*",
    AERO_L2_AUTH_MODE: "token",
    AERO_L2_TOKEN: "sekrit",
    AERO_L2_MAX_CONNECTIONS: "0",
  });

  try {
    const missingBoth = await connectOrReject(`ws://127.0.0.1:${proxy.port}/l2`);
    assert.equal(missingBoth.ok, false);
    assert.equal(missingBoth.status, 401);

    const missingOrigin = await connectOrReject(`ws://127.0.0.1:${proxy.port}/l2?token=sekrit`);
    assert.equal(missingOrigin.ok, false);
    assert.equal(missingOrigin.status, 403);
  } finally {
    await proxy.close();
  }
});

test("AERO_L2_OPEN disables Origin enforcement (but not token auth)", { timeout: L2_PROXY_TEST_TIMEOUT_MS }, async () => {
  const proxy = await startRustL2Proxy({
    AERO_L2_OPEN: "1",
    AERO_L2_ALLOWED_ORIGINS: "",
    AERO_L2_AUTH_MODE: "token",
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

test("cookie auth requires a valid aero_session cookie", { timeout: L2_PROXY_TEST_TIMEOUT_MS }, async () => {
  const proxy = await startRustL2Proxy({
    AERO_L2_OPEN: "1",
    AERO_L2_ALLOWED_ORIGINS: "",
    AERO_L2_TOKEN: "",
    AERO_L2_AUTH_MODE: "session",
    AERO_L2_SESSION_SECRET: "sekrit",
    AERO_L2_MAX_CONNECTIONS: "0",
  });

  try {
    const missing = await connectOrReject(`ws://127.0.0.1:${proxy.port}/l2`);
    assert.equal(missing.ok, false);
    assert.equal(missing.status, 401);

    const cookie = makeSessionCookie({
      secret: "sekrit",
      sid: "sid-test",
      expUnixSeconds: Math.floor(Date.now() / 1000) + 3600,
    });
    const ok = await connectOrReject(`ws://127.0.0.1:${proxy.port}/l2`, {
      headers: { cookie },
    });
    assert.equal(ok.ok, true);
    ok.ws.close(1000, "done");
    await waitForClose(ok.ws);

    const expiredCookie = makeSessionCookie({
      secret: "sekrit",
      sid: "sid-test",
      expUnixSeconds: 0,
    });
    const expired = await connectOrReject(`ws://127.0.0.1:${proxy.port}/l2`, {
      headers: { cookie: expiredCookie },
    });
    assert.equal(expired.ok, false);
    assert.equal(expired.status, 401);
  } finally {
    await proxy.close();
  }
});

test("token auth mode accepts ?apiKey= and subprotocol credentials", { timeout: L2_PROXY_TEST_TIMEOUT_MS }, async () => {
  const proxy = await startRustL2Proxy({
    AERO_L2_OPEN: "1",
    AERO_L2_ALLOWED_ORIGINS: "",
    AERO_L2_TOKEN: "",
    AERO_L2_AUTH_MODE: "token",
    AERO_L2_API_KEY: "sekrit",
    AERO_L2_MAX_CONNECTIONS: "0",
  });

  try {
    const missing = await connectOrReject(`ws://127.0.0.1:${proxy.port}/l2`);
    assert.equal(missing.ok, false);
    assert.equal(missing.status, 401);

    const wrong = await connectOrReject(`ws://127.0.0.1:${proxy.port}/l2?apiKey=nope`);
    assert.equal(wrong.ok, false);
    assert.equal(wrong.status, 401);

    const ok = await connectOrReject(`ws://127.0.0.1:${proxy.port}/l2?apiKey=sekrit`);
    assert.equal(ok.ok, true);
    ok.ws.close(1000, "done");
    await waitForClose(ok.ws);

    const protoOk = await connectOrReject(`ws://127.0.0.1:${proxy.port}/l2`, {
      protocols: [L2_TUNNEL_SUBPROTOCOL, `${L2_TUNNEL_TOKEN_SUBPROTOCOL_PREFIX}sekrit`],
    });
    assert.equal(protoOk.ok, true);
    protoOk.ws.close(1000, "done");
    await waitForClose(protoOk.ws);
  } finally {
    await proxy.close();
  }
});

test("jwt auth mode accepts Authorization: Bearer tokens", { timeout: L2_PROXY_TEST_TIMEOUT_MS }, async () => {
  const nowUnixSeconds = Math.floor(Date.now() / 1000);
  const jwt = makeJwtHs256({
    secret: "sekrit",
    claims: {
      iat: nowUnixSeconds,
      exp: nowUnixSeconds + 3600,
      sid: "sid-test",
    },
  });

  const proxy = await startRustL2Proxy({
    AERO_L2_OPEN: "1",
    AERO_L2_ALLOWED_ORIGINS: "",
    AERO_L2_TOKEN: "",
    AERO_L2_AUTH_MODE: "jwt",
    AERO_L2_JWT_SECRET: "sekrit",
    AERO_L2_MAX_CONNECTIONS: "0",
  });

  try {
    const missing = await connectOrReject(`ws://127.0.0.1:${proxy.port}/l2`);
    assert.equal(missing.ok, false);
    assert.equal(missing.status, 401);

    const invalid = await connectOrReject(`ws://127.0.0.1:${proxy.port}/l2`, {
      headers: { authorization: "Bearer not-a-jwt" },
    });
    assert.equal(invalid.ok, false);
    assert.equal(invalid.status, 401);

    const ok = await connectOrReject(`ws://127.0.0.1:${proxy.port}/l2`, {
      headers: { authorization: `Bearer ${jwt}` },
    });
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
    AERO_L2_AUTH_MODE: "none",
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

test("l2 proxy enforces per-session tunnel quota (cookie auth)", { timeout: L2_PROXY_TEST_TIMEOUT_MS }, async () => {
  const proxy = await startRustL2Proxy({
    AERO_L2_OPEN: "1",
    AERO_L2_ALLOWED_ORIGINS: "",
    AERO_L2_TOKEN: "",
    AERO_L2_AUTH_MODE: "session",
    AERO_L2_SESSION_SECRET: "sekrit",
    AERO_L2_MAX_CONNECTIONS: "0",
    AERO_L2_MAX_TUNNELS_PER_SESSION: "1",
  });

  try {
    const cookie = makeSessionCookie({
      secret: "sekrit",
      sid: "sid-test",
      expUnixSeconds: Math.floor(Date.now() / 1000) + 3600,
    });

    const first = await connectOrReject(`ws://127.0.0.1:${proxy.port}/l2`, {
      headers: { cookie },
    });
    assert.equal(first.ok, true);

    const second = await connectOrReject(`ws://127.0.0.1:${proxy.port}/l2`, {
      headers: { cookie },
    });
    assert.equal(second.ok, false);
    assert.equal(second.status, 429);

    first.ws.close(1000, "done");
    await waitForClose(first.ws);
    // The per-session tunnel permit is released when the session task exits. Avoid a fixed sleep
    // here; under CI load the permit release can lag slightly behind the client observing the
    // close frame.
    const deadline = Date.now() + 2_000;
    let third = null;
    while (Date.now() < deadline) {
      // eslint-disable-next-line no-await-in-loop
      const attempt = await connectOrReject(`ws://127.0.0.1:${proxy.port}/l2`, {
        headers: { cookie },
      });
      if (attempt.ok) {
        third = attempt;
        break;
      }
      assert.equal(attempt.status, 429);
      // eslint-disable-next-line no-await-in-loop
      await sleep(25);
    }
    assert.ok(third && third.ok, "expected tunnel permit to be released after closing the first session");

    assert.equal(third.ok, true);
    third.ws.close(1000, "done");
    await waitForClose(third.ws);
  } finally {
    await proxy.close();
  }
});

test("l2 proxy closes the socket when per-connection quotas are exceeded", { timeout: L2_PROXY_TEST_TIMEOUT_MS }, async () => {
  const proxy = await startRustL2Proxy({
    AERO_L2_OPEN: "1",
    AERO_L2_ALLOWED_ORIGINS: "",
    AERO_L2_AUTH_MODE: "none",
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
