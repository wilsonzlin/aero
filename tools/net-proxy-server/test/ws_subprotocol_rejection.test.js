import assert from "node:assert/strict";
import test from "node:test";
import { randomBytes } from "node:crypto";

import { createProxyServer } from "../src/server.js";
import { TCP_MUX_SUBPROTOCOL } from "../src/protocol.js";
import { sendRawHttpRequest } from "../../../tests/helpers/http_raw_response.js";

async function sendRawUpgradeRequest(host, port, request) {
  return await sendRawHttpRequest(host, port, request);
}

function makeKey() {
  return randomBytes(16).toString("base64");
}

test("upgrade: missing Sec-WebSocket-Protocol rejects with Missing required subprotocol", async () => {
  const proxy = await createProxyServer({
    host: "127.0.0.1",
    port: 0,
    authToken: "test-token",
    allowPrivateIps: true,
    metricsIntervalMs: 0,
  });

  try {
    const url = new URL(proxy.url);
    const res = await sendRawUpgradeRequest(
      url.hostname,
      Number(url.port),
      [
        `GET ${url.pathname}?token=test-token HTTP/1.1`,
        `Host: ${url.host}`,
        "Connection: Upgrade",
        "Upgrade: websocket",
        "Sec-WebSocket-Version: 13",
        `Sec-WebSocket-Key: ${makeKey()}`,
        "",
        "",
      ].join("\r\n"),
    );
    assert.ok(res.statusLine.startsWith("HTTP/1.1 400 "));
    assert.equal(res.headers["cache-control"], "no-store");
    assert.ok(res.headers["content-length"]);
    assert.equal(res.body.length, Number.parseInt(res.headers["content-length"], 10));
    assert.ok(res.body.toString("utf8").includes(`Missing required subprotocol: ${TCP_MUX_SUBPROTOCOL}`));
  } finally {
    await proxy.close();
  }
});

test("upgrade: oversized Sec-WebSocket-Protocol rejects as invalid header (not missing)", async () => {
  const proxy = await createProxyServer({
    host: "127.0.0.1",
    port: 0,
    authToken: "test-token",
    allowPrivateIps: true,
    metricsIntervalMs: 0,
  });

  try {
    const url = new URL(proxy.url);
    const huge = `${TCP_MUX_SUBPROTOCOL}, ${"a".repeat(5000)}`;
    const res = await sendRawUpgradeRequest(
      url.hostname,
      Number(url.port),
      [
        `GET ${url.pathname}?token=test-token HTTP/1.1`,
        `Host: ${url.host}`,
        "Connection: Upgrade",
        "Upgrade: websocket",
        "Sec-WebSocket-Version: 13",
        `Sec-WebSocket-Key: ${makeKey()}`,
        `Sec-WebSocket-Protocol: ${huge}`,
        "",
        "",
      ].join("\r\n"),
    );
    assert.ok(res.statusLine.startsWith("HTTP/1.1 400 "));
    assert.equal(res.headers["cache-control"], "no-store");
    assert.ok(res.headers["content-length"]);
    assert.equal(res.body.length, Number.parseInt(res.headers["content-length"], 10));
    assert.ok(res.body.toString("utf8").includes("Invalid Sec-WebSocket-Protocol header"));
  } finally {
    await proxy.close();
  }
});

test("upgrade: invalid Sec-WebSocket-Protocol tokens reject as invalid header (not missing)", async () => {
  const proxy = await createProxyServer({
    host: "127.0.0.1",
    port: 0,
    authToken: "test-token",
    allowPrivateIps: true,
    metricsIntervalMs: 0,
  });

  try {
    const url = new URL(proxy.url);
    const res = await sendRawUpgradeRequest(
      url.hostname,
      Number(url.port),
      [
        `GET ${url.pathname}?token=test-token HTTP/1.1`,
        `Host: ${url.host}`,
        "Connection: Upgrade",
        "Upgrade: websocket",
        "Sec-WebSocket-Version: 13",
        `Sec-WebSocket-Key: ${makeKey()}`,
        "Sec-WebSocket-Protocol: a b",
        "",
        "",
      ].join("\r\n"),
    );
    assert.ok(res.statusLine.startsWith("HTTP/1.1 400 "));
    assert.equal(res.headers["cache-control"], "no-store");
    assert.ok(res.headers["content-length"]);
    assert.equal(res.body.length, Number.parseInt(res.headers["content-length"], 10));
    assert.ok(res.body.toString("utf8").includes("Invalid Sec-WebSocket-Protocol header"));
  } finally {
    await proxy.close();
  }
});

