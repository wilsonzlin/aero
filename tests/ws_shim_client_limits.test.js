import test from "node:test";
import assert from "node:assert/strict";

import { WebSocket } from "../scripts/ws-shim.mjs";

test("ws-shim client: rejects oversized URL before parsing/network", () => {
  const url = "ws://example.com/" + "a".repeat(8 * 1024);
  assert.throws(() => new WebSocket(url), /too long/i);
});

test("ws-shim client: rejects oversized Sec-WebSocket-Protocol before network", () => {
  const proto = "x".repeat(1024);
  const protocols = Array.from({ length: 8 }, () => proto); // >4KiB once joined
  assert.throws(() => new WebSocket("ws://example.com/", protocols), /protocol/i);
});

test("ws-shim client: rejects invalid subprotocol tokens", () => {
  assert.throws(() => new WebSocket("ws://example.com/", ["a b"]), TypeError);
  assert.throws(() => new WebSocket("ws://example.com/", ["a,b"]), TypeError);
});

