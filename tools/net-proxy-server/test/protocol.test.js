import assert from "node:assert/strict";
import test from "node:test";
import {
  decodeFrame,
  encodeClose,
  encodeData,
  encodeError,
  encodeOpenAck,
  encodeOpenRequest,
  ErrorCode,
  FrameType,
} from "../src/protocol.js";

test("protocol: OPEN request encode/decode", () => {
  const frame = encodeOpenRequest(123, new Uint8Array([1, 2, 3, 4]), 8080);
  const decoded = decodeFrame(frame);
  assert.equal(decoded.type, FrameType.OPEN);
  assert.equal(decoded.connectionId, 123);
  assert.equal(decoded.kind, "request");
  assert.equal(decoded.ipVersion, 4);
  assert.deepEqual(Array.from(decoded.dstIp), [1, 2, 3, 4]);
  assert.equal(decoded.dstPort, 8080);
});

test("protocol: OPEN ack encode/decode", () => {
  const frame = encodeOpenAck(7);
  const decoded = decodeFrame(frame);
  assert.equal(decoded.type, FrameType.OPEN);
  assert.equal(decoded.connectionId, 7);
  assert.equal(decoded.kind, "ack");
});

test("protocol: DATA encode/decode", () => {
  const payload = new Uint8Array([9, 8, 7]);
  const frame = encodeData(42, payload);
  const decoded = decodeFrame(frame);
  assert.equal(decoded.type, FrameType.DATA);
  assert.equal(decoded.connectionId, 42);
  assert.deepEqual(Array.from(decoded.data), [9, 8, 7]);
});

test("protocol: CLOSE encode/decode", () => {
  const frame = encodeClose(9);
  const decoded = decodeFrame(frame);
  assert.equal(decoded.type, FrameType.CLOSE);
  assert.equal(decoded.connectionId, 9);
});

test("protocol: ERROR encode/decode", () => {
  const frame = encodeError(9, ErrorCode.POLICY_DENIED, "nope");
  const decoded = decodeFrame(frame);
  assert.equal(decoded.type, FrameType.ERROR);
  assert.equal(decoded.connectionId, 9);
  assert.equal(decoded.code, ErrorCode.POLICY_DENIED);
  assert.equal(decoded.message, "nope");
});

