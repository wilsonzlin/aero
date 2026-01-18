import assert from "node:assert/strict";
import test from "node:test";

import {
  computeWebSocketAccept,
  encodeWebSocketHandshakeResponse,
  writeWebSocketHandshake,
} from "../src/ws_handshake_response.js";

test("ws_handshake_response: computeWebSocketAccept matches RFC6455 example", () => {
  // RFC6455 example:
  // Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==
  // Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=
  assert.equal(computeWebSocketAccept("dGhlIHNhbXBsZSBub25jZQ=="), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
});

test("ws_handshake_response: encodeWebSocketHandshakeResponse frames headers with one blank line", () => {
  const text = encodeWebSocketHandshakeResponse({ key: "dGhlIHNhbXBsZSBub25jZQ==" });
  assert.ok(text.startsWith("HTTP/1.1 101 Switching Protocols\r\n"));
  assert.ok(text.includes("\r\nUpgrade: websocket\r\n"));
  assert.ok(text.includes("\r\nConnection: Upgrade\r\n"));
  assert.ok(text.includes("\r\nSec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n"));
  assert.ok(text.endsWith("\r\n\r\n"));
  assert.ok(!text.includes("\r\n\r\n\r\n"));
});

test("ws_handshake_response: includes Sec-WebSocket-Protocol only for valid tokens", () => {
  const ok = encodeWebSocketHandshakeResponse({ key: "k", protocol: "chat" });
  assert.ok(ok.includes("\r\nSec-WebSocket-Protocol: chat\r\n"));

  const bad = encodeWebSocketHandshakeResponse({ key: "k", protocol: "bad token" });
  assert.ok(!bad.includes("\r\nSec-WebSocket-Protocol: "));
});

test("ws_handshake_response: writeWebSocketHandshake destroys socket if write throws", () => {
  /** @type {string[]} */
  const calls = [];
  const socket = {
    write() {
      calls.push("write");
      throw new Error("boom");
    },
    destroy() {
      calls.push("destroy");
    },
  };

  writeWebSocketHandshake(socket, { key: "k", protocol: "chat" });
  assert.deepEqual(calls, ["write", "destroy"]);
});

test("ws_handshake_response: computeWebSocketAccept rejects empty key", () => {
  assert.throws(() => computeWebSocketAccept(""));
  assert.throws(() => computeWebSocketAccept(/** @type {any} */ (null)));
});

