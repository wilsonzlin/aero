import assert from "node:assert/strict";
import net from "node:net";
import test from "node:test";
import { randomBytes } from "node:crypto";

import { createProxyServer } from "../src/server.js";
import { TCP_MUX_SUBPROTOCOL } from "../src/protocol.js";

function sendRawUpgradeRequest(host, port, request) {
  return new Promise((resolve, reject) => {
    const socket = net.connect({ host, port });
    const chunks = [];

    const cleanup = () => {
      socket.removeAllListeners();
      try {
        socket.destroy();
      } catch {
        // ignore
      }
    };

    socket.on("error", (err) => {
      cleanup();
      reject(err);
    });

    socket.on("data", (chunk) => {
      chunks.push(Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk));
      const text = Buffer.concat(chunks).toString("utf8");
      if (text.includes("\r\n\r\n")) {
        cleanup();
        resolve(text);
      }
    });

    socket.write(request);
  });
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
        "\r\n",
      ].join("\r\n"),
    );
    assert.ok(res.startsWith("HTTP/1.1 400 "));
    assert.ok(res.includes(`Missing required subprotocol: ${TCP_MUX_SUBPROTOCOL}`));
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
        "\r\n",
      ].join("\r\n"),
    );
    assert.ok(res.startsWith("HTTP/1.1 400 "));
    assert.ok(res.includes("Invalid Sec-WebSocket-Protocol header"));
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
        "\r\n",
      ].join("\r\n"),
    );
    assert.ok(res.startsWith("HTTP/1.1 400 "));
    assert.ok(res.includes("Invalid Sec-WebSocket-Protocol header"));
  } finally {
    await proxy.close();
  }
});

