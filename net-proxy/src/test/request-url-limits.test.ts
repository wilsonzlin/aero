import test from "node:test";
import assert from "node:assert/strict";
import http from "node:http";
import { startProxyServer } from "../server";

function httpGetJson(
  host: string,
  port: number,
  path: string
): Promise<{ status: number; body: unknown }> {
  return new Promise((resolve, reject) => {
    const req = http.request({ method: "GET", host, port, path }, (res) => {
      const chunks: Buffer[] = [];
      res.on("data", (chunk) => chunks.push(Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk)));
      res.on("end", () => {
        const raw = Buffer.concat(chunks).toString("utf8");
        let body: unknown = raw;
        try {
          body = raw.length === 0 ? null : (JSON.parse(raw) as unknown);
        } catch {
          // keep raw string body
        }
        resolve({ status: res.statusCode ?? 0, body });
      });
    });
    req.on("error", reject);
    req.end();
  });
}

test("HTTP endpoints reject overly long request URLs (414)", async () => {
  const proxy = await startProxyServer({ listenHost: "127.0.0.1", listenPort: 0, open: true });
  const addr = proxy.server.address();
  assert.ok(addr && typeof addr !== "string");

  try {
    // Large enough to exceed `MAX_REQUEST_URL_LEN`, but small enough to be accepted by Node's
    // core HTTP parser in typical configurations.
    const path = `/${"a".repeat(9_000)}`;
    const res = await httpGetJson("127.0.0.1", addr.port, path);
    assert.equal(res.status, 414);
    assert.deepEqual(res.body, { error: "url too long" });
  } finally {
    await proxy.close();
  }
});

