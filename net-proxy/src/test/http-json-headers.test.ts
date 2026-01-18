import test from "node:test";
import assert from "node:assert/strict";
import http from "node:http";

import { startProxyServer } from "../server";

function httpGetRaw(
  host: string,
  port: number,
  path: string
): Promise<{ status: number; headers: http.IncomingHttpHeaders; body: Buffer }> {
  return new Promise((resolve, reject) => {
    const req = http.request({ method: "GET", host, port, path }, (res) => {
      const chunks: Buffer[] = [];
      res.on("data", (chunk) => chunks.push(Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk)));
      res.on("end", () => resolve({ status: res.statusCode ?? 0, headers: res.headers, body: Buffer.concat(chunks) }));
    });
    req.on("error", reject);
    req.end();
  });
}

function headerSingle(headers: http.IncomingHttpHeaders, name: string): string | null {
  const v = headers[name.toLowerCase() as keyof http.IncomingHttpHeaders];
  if (typeof v === "string") return v;
  if (Array.isArray(v)) {
    if (v.length !== 1) return null;
    return typeof v[0] === "string" ? v[0] : null;
  }
  return null;
}

function assertJsonResponseHeaders(headers: http.IncomingHttpHeaders, body: Buffer): void {
  assert.equal(headerSingle(headers, "cache-control"), "no-store");
  assert.equal(headerSingle(headers, "content-type"), "application/json; charset=utf-8");

  const rawLen = headerSingle(headers, "content-length");
  if (rawLen === null) {
    throw new Error("missing/invalid Content-Length header");
  }
  const len = Number.parseInt(rawLen, 10);
  assert.ok(Number.isFinite(len) && len >= 0);
  assert.equal(body.length, len);
}

test("HTTP JSON endpoints include Content-Length and Cache-Control", async () => {
  const proxy = await startProxyServer({ listenHost: "127.0.0.1", listenPort: 0, open: true });
  const addr = proxy.server.address();
  assert.ok(addr && typeof addr !== "string");

  try {
    const health = await httpGetRaw("127.0.0.1", addr.port, "/healthz");
    assert.equal(health.status, 200);
    assertJsonResponseHeaders(health.headers, health.body);
    assert.deepEqual(JSON.parse(health.body.toString("utf8")), { ok: true });

    const missing = await httpGetRaw("127.0.0.1", addr.port, "/nope");
    assert.equal(missing.status, 404);
    assertJsonResponseHeaders(missing.headers, missing.body);
    assert.deepEqual(JSON.parse(missing.body.toString("utf8")), { error: "not found" });
  } finally {
    await proxy.close();
  }
});

test("HTTP JSON responses do not crash if JSON.stringify throws", async () => {
  const proxy = await startProxyServer({ listenHost: "127.0.0.1", listenPort: 0, open: true });
  const addr = proxy.server.address();
  assert.ok(addr && typeof addr !== "string");

  const original = JSON.stringify;
  try {
    // Simulate a hostile/corrupt environment: stringify always throws.
    JSON.stringify = () => {
      throw new Error("boom");
    };

    const res = await httpGetRaw("127.0.0.1", addr.port, "/healthz");
    assert.equal(res.status, 500);
    assertJsonResponseHeaders(res.headers, res.body);
    assert.deepEqual(JSON.parse(res.body.toString("utf8")), { error: "internal server error" });

    // /dns-json uses its own handler path; verify it also fails closed instead of throwing.
    const res2 = await httpGetRaw("127.0.0.1", addr.port, "/dns-json?name=localhost&type=A");
    assert.equal(res2.status, 500);
    assertJsonResponseHeaders(res2.headers, res2.body);
    assert.deepEqual(JSON.parse(res2.body.toString("utf8")), { error: "internal server error" });
  } finally {
    JSON.stringify = original;
    await proxy.close();
  }
});
