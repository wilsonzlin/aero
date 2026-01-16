import assert from "node:assert/strict";
import test from "node:test";

import { hasWebSocketSubprotocol } from "../wsSubprotocol";

test("hasWebSocketSubprotocol: absent header is ok+false", () => {
  assert.deepEqual(hasWebSocketSubprotocol(undefined, "aero-tcp-mux-v1"), { ok: true, has: false });
});

test("hasWebSocketSubprotocol: finds required token among comma-separated list (with whitespace)", () => {
  const header = "chat, aero-tcp-mux-v1, superchat";
  assert.deepEqual(hasWebSocketSubprotocol(header, "aero-tcp-mux-v1"), { ok: true, has: true });
});

test("hasWebSocketSubprotocol: supports string[] headers and enforces total length cap", () => {
  const parts = ["aero-tcp-mux-v1", "x".repeat(4096)];
  assert.deepEqual(hasWebSocketSubprotocol(parts, "aero-tcp-mux-v1"), { ok: false, has: false });
});

test("hasWebSocketSubprotocol: rejects non-string elements in string[] header", () => {
  const parts = ["aero-tcp-mux-v1", 123] as unknown as string[];
  assert.deepEqual(hasWebSocketSubprotocol(parts, "aero-tcp-mux-v1"), { ok: false, has: false });
});

test("hasWebSocketSubprotocol: rejects too many protocol tokens", () => {
  const tokens = Array.from({ length: 33 }, (_v, i) => `p${i}`);
  assert.deepEqual(hasWebSocketSubprotocol(tokens.join(","), "aero-tcp-mux-v1"), { ok: false, has: false });
});

test("hasWebSocketSubprotocol: does not accept partial token matches", () => {
  assert.deepEqual(hasWebSocketSubprotocol("aero-tcp-mux-v1x", "aero-tcp-mux-v1"), { ok: true, has: false });
});

test("hasWebSocketSubprotocol: rejects invalid token characters", () => {
  assert.deepEqual(hasWebSocketSubprotocol("a b", "aero-tcp-mux-v1"), { ok: false, has: false });
});

