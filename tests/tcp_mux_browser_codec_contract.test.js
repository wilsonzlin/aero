import assert from "node:assert/strict";
import test from "node:test";

import {
  decodeTcpMuxErrorPayload,
  MAX_TCP_MUX_ERROR_MESSAGE_BYTES,
  TcpMuxErrorCode,
} from "../web/src/net/tcpMuxProxy.ts";

test("tcp-mux browser codec: ERROR rejects invalid UTF-8 message", () => {
  const invalidUtf8 = new Uint8Array([0xc0, 0xaf]);
  const payload = new Uint8Array(2 + 2 + invalidUtf8.length);
  const dv = new DataView(payload.buffer);
  dv.setUint16(0, TcpMuxErrorCode.POLICY_DENIED, false);
  dv.setUint16(2, invalidUtf8.length, false);
  payload.set(invalidUtf8, 4);
  assert.throws(() => decodeTcpMuxErrorPayload(payload), /message is not valid utf-8/i);
});

test("tcp-mux browser codec: ERROR rejects oversized message length", () => {
  const msgLen = MAX_TCP_MUX_ERROR_MESSAGE_BYTES + 1;
  const payload = new Uint8Array(2 + 2 + msgLen);
  const dv = new DataView(payload.buffer);
  dv.setUint16(0, TcpMuxErrorCode.POLICY_DENIED, false);
  dv.setUint16(2, msgLen, false);
  assert.throws(() => decodeTcpMuxErrorPayload(payload), /error message too long/i);
});

