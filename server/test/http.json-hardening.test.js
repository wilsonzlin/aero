import test from "node:test";
import assert from "node:assert/strict";
import http from "node:http";

import { createAeroServer } from "../src/server.js";
import { resolveConfig } from "../src/config.js";

async function listen(server) {
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const address = server.address();
  return address.port;
}

async function closeServer(server) {
  await new Promise((resolve) => server.close(resolve));
}

async function httpGet({ port, path }) {
  return await new Promise((resolve, reject) => {
    const req = http.request(
      {
        host: "127.0.0.1",
        port,
        method: "GET",
        path,
        headers: {
          Connection: "close",
        },
      },
      (res) => {
        /** @type {Buffer[]} */
        const chunks = [];
        res.on("data", (chunk) => chunks.push(Buffer.from(chunk)));
        res.on("end", () => {
          resolve({
            statusCode: res.statusCode,
            headers: res.headers,
            body: Buffer.concat(chunks),
          });
        });
      },
    );
    req.on("error", reject);
    req.end();
  });
}

test("http: returns stable 500 JSON when JSON.stringify throws (and server stays alive)", async () => {
  const token = "test-token";
  const config = resolveConfig({
    host: "127.0.0.1",
    port: 0,
    tokens: [token],
    allowHosts: [{ kind: "wildcard" }],
    allowPrivateRanges: true,
  });

  const { httpServer } = createAeroServer(config);
  const port = await listen(httpServer);

  const originalStringify = JSON.stringify;
  try {
    // Force the server's sendJson() implementation down its defensive path.
    JSON.stringify = () => {
      throw new Error("boom");
    };

    const res = await httpGet({
      port,
      path: `/api/dns/lookup?token=${encodeURIComponent(token)}&name=localhost`,
    });
    assert.equal(res.statusCode, 500);
    assert.equal(res.headers["content-type"], "application/json; charset=utf-8");
    assert.equal(res.headers["cache-control"], "no-store");

    const body = res.body.toString("utf8");
    assert.equal(body, `{"error":"internal server error"}\n`);
    assert.equal(Number(res.headers["content-length"]), Buffer.byteLength(body, "utf8"));

    JSON.stringify = originalStringify;
    const health = await httpGet({ port, path: "/healthz" });
    assert.equal(health.statusCode, 200);
    assert.equal(health.body.toString("utf8"), "ok");
  } finally {
    JSON.stringify = originalStringify;
    await closeServer(httpServer);
  }
});

