import assert from "node:assert/strict";
import test from "node:test";

import type http from "node:http";

import { validateWebSocketHandshakeRequest } from "../wsUpgradeRequest";

function req(headers: http.IncomingHttpHeaders): http.IncomingMessage {
  return { headers } as unknown as http.IncomingMessage;
}

test("validateWebSocketHandshakeRequest accepts token-lists (comma-separated) for Upgrade/Connection", () => {
  const decision = validateWebSocketHandshakeRequest(
    req({
      upgrade: "h2c, WebSocket",
      connection: "keep-alive, Upgrade",
      "sec-websocket-version": "13",
      "sec-websocket-key": "abc",
    }),
  );
  assert.deepEqual(decision, { ok: true });
});

test("validateWebSocketHandshakeRequest rejects partial token matches", () => {
  const decision = validateWebSocketHandshakeRequest(
    req({
      upgrade: "websocket2",
      connection: "upgrade",
      "sec-websocket-version": "13",
      "sec-websocket-key": "abc",
    }),
  );
  assert.deepEqual(decision, { ok: false, status: 400, message: "Invalid WebSocket upgrade" });
});

