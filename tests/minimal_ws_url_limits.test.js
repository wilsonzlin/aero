import test from "node:test";
import assert from "node:assert/strict";
import { once } from "node:events";

import { WebSocket } from "../tools/minimal_ws.js";

test("minimal_ws: rejects overly long websocket URLs", () => {
  const longUrl = `ws://example.com/${"a".repeat(10_000)}`;
  assert.throws(() => new WebSocket(longUrl), RangeError);
});

test("minimal_ws: rejects invalid URL strings", () => {
  assert.throws(() => new WebSocket("not-a-url"), TypeError);
});

test("minimal_ws: emits error for unsupported URL schemes", async () => {
  const ws = new WebSocket("http://example.com");
  const [err] = await once(ws, "error");
  assert.ok(err instanceof Error);
  assert.match(err.message, /Unsupported WebSocket URL scheme/i);
});

test("minimal_ws: accepts URL objects", async () => {
  const ws = new WebSocket(new URL("http://example.com"));
  const [err] = await once(ws, "error");
  assert.ok(err instanceof Error);
  assert.match(err.message, /Unsupported WebSocket URL scheme/i);
});

test("minimal_ws: rejects too many subprotocols", () => {
  const protocols = Array.from({ length: 100 }, (_, i) => `p${i}`);
  assert.throws(() => new WebSocket("ws://example.com", protocols), RangeError);
});

test("minimal_ws: rejects oversized subprotocol headers", () => {
  assert.throws(() => new WebSocket("ws://example.com", ["a".repeat(10_000)]), RangeError);
});

test("minimal_ws: rejects invalid subprotocol tokens", () => {
  assert.throws(() => new WebSocket("ws://example.com", ["a b"]), TypeError);
  assert.throws(() => new WebSocket("ws://example.com", ["a,b"]), TypeError);
});

test("minimal_ws: rejects overly long websocket URL objects", () => {
  const longUrl = new URL(`ws://example.com/${"a".repeat(10_000)}`);
  assert.throws(() => new WebSocket(longUrl), RangeError);
});

