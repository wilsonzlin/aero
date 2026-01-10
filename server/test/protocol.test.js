import test from "node:test";
import assert from "node:assert/strict";

import {
  AddrType,
  FrameType,
  decodeClientFrame,
  decodeServerFrame,
  encodeClientDataFrame,
  encodeConnectFrame,
  encodeOpenedFrame,
} from "../src/protocol.js";

test("CONNECT framing round-trips (hostname)", () => {
  const buf = encodeConnectFrame({ connId: 42, host: "example.com", port: 443 });
  assert.equal(buf.readUInt8(0), FrameType.CONNECT);
  assert.equal(buf.readUInt32BE(1), 42);
  assert.equal(buf.readUInt8(5), AddrType.HOSTNAME);

  const frame = decodeClientFrame(buf);
  assert.deepEqual(frame, { type: "connect", connId: 42, host: "example.com", port: 443 });
});

test("CONNECT framing round-trips (ipv4)", () => {
  const buf = encodeConnectFrame({ connId: 1, host: "1.2.3.4", port: 80 });
  assert.equal(buf.readUInt8(5), AddrType.IPV4);
  const frame = decodeClientFrame(buf);
  assert.deepEqual(frame, { type: "connect", connId: 1, host: "1.2.3.4", port: 80 });
});

test("DATA framing round-trips", () => {
  const buf = encodeClientDataFrame({ connId: 7, data: Buffer.from("hello") });
  const frame = decodeClientFrame(buf);
  assert.equal(frame.type, "data");
  assert.equal(frame.connId, 7);
  assert.equal(frame.data.toString("utf8"), "hello");
});

test("server OPENED framing round-trips", () => {
  const buf = encodeOpenedFrame({ connId: 9, status: 3, message: "nope" });
  const frame = decodeServerFrame(buf);
  assert.deepEqual(frame, { type: "opened", connId: 9, status: 3, message: "nope" });
});

test("invalid frames are rejected", () => {
  assert.throws(() => decodeClientFrame(Buffer.from([])), /Frame too short/);
  assert.throws(() => decodeClientFrame(Buffer.from([FrameType.CONNECT, 0, 0, 0, 1])), /Frame too short/);
});

