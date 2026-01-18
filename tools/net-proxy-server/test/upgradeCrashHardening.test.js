import assert from "node:assert/strict";
import test from "node:test";
import { randomBytes } from "node:crypto";
import { WebSocketServer } from "ws";

import { createProxyServer } from "../src/server.js";
import { TCP_MUX_SUBPROTOCOL } from "../src/protocol.js";
import { sendRawHttpRequest } from "../../../tests/helpers/http_raw_response.js";

function makeKey() {
  return randomBytes(16).toString("base64");
}

test("upgrade: returns 500 when ws handleUpgrade throws (and server stays alive)", async (t) => {
  const proxy = await createProxyServer({
    host: "127.0.0.1",
    port: 0,
    authToken: "test-token",
    allowPrivateIps: true,
    metricsIntervalMs: 0,
  });

  t.mock.method(WebSocketServer.prototype, "handleUpgrade", () => {
    throw new Error("boom");
  });

  try {
    const url = new URL(proxy.url);
    const res = await sendRawHttpRequest(
      url.hostname,
      Number(url.port),
      [
        `GET ${url.pathname}?token=test-token HTTP/1.1`,
        `Host: ${url.host}`,
        "Connection: Upgrade",
        "Upgrade: websocket",
        "Sec-WebSocket-Version: 13",
        `Sec-WebSocket-Key: ${makeKey()}`,
        `Sec-WebSocket-Protocol: ${TCP_MUX_SUBPROTOCOL}`,
        "",
        "",
      ].join("\r\n"),
    );

    assert.ok(res.statusLine.startsWith("HTTP/1.1 500 Internal Server Error"), res.statusLine);
    assert.equal(res.headers["cache-control"], "no-store");
    assert.ok(res.headers["content-length"]);
    assert.equal(res.body.length, Number.parseInt(res.headers["content-length"], 10));
    assert.ok(res.body.toString("utf8").includes("WebSocket upgrade failed"));

    const res2 = await sendRawHttpRequest(
      url.hostname,
      Number(url.port),
      [`GET / HTTP/1.1`, `Host: ${url.host}`, "Connection: close", "", ""].join("\r\n"),
    );
    assert.ok(res2.statusLine.startsWith("HTTP/1.1 404 "), res2.statusLine);
  } finally {
    await proxy.close();
  }
});

