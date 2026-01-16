import assert from "node:assert/strict";
import test from "node:test";

import {
  TCP_MUX_HEADER_BYTES,
  MAX_TCP_MUX_OPEN_HOST_BYTES,
  MAX_TCP_MUX_OPEN_METADATA_BYTES,
  TcpMuxCloseFlags,
  TcpMuxErrorCode,
  TcpMuxFrameParser,
  TcpMuxMsgType,
  decodeTcpMuxClosePayload,
  decodeTcpMuxErrorPayload,
  decodeTcpMuxOpenPayload,
  encodeTcpMuxClosePayload,
  encodeTcpMuxErrorPayload,
  encodeTcpMuxFrame,
  encodeTcpMuxOpenPayload,
} from "../src/protocol.js";

test("tcp-mux: OPEN payload encode/decode (host + port + metadata)", () => {
  const payload = encodeTcpMuxOpenPayload({ host: "example.com", port: 443, metadata: '{"k":"v"}' });
  const decoded = decodeTcpMuxOpenPayload(payload);
  assert.deepEqual(decoded, { host: "example.com", port: 443, metadata: '{"k":"v"}' });
});

test("tcp-mux: OPEN payload rejects invalid utf8 and whitespace in host", () => {
  const hostWithSpace = Buffer.from("a b", "utf8");
  const payloadSpace = Buffer.allocUnsafe(2 + hostWithSpace.length + 2 + 2);
  let off = 0;
  payloadSpace.writeUInt16BE(hostWithSpace.length, off);
  off += 2;
  hostWithSpace.copy(payloadSpace, off);
  off += hostWithSpace.length;
  payloadSpace.writeUInt16BE(80, off);
  off += 2;
  payloadSpace.writeUInt16BE(0, off); // metadata_len
  assert.throws(() => decodeTcpMuxOpenPayload(payloadSpace), /invalid host/i);

  const invalidUtf8 = Buffer.from([0xc0, 0xaf]);
  const payloadInvalidUtf8 = Buffer.allocUnsafe(2 + invalidUtf8.length + 2 + 2);
  off = 0;
  payloadInvalidUtf8.writeUInt16BE(invalidUtf8.length, off);
  off += 2;
  invalidUtf8.copy(payloadInvalidUtf8, off);
  off += invalidUtf8.length;
  payloadInvalidUtf8.writeUInt16BE(80, off);
  off += 2;
  payloadInvalidUtf8.writeUInt16BE(0, off); // metadata_len
  assert.throws(() => decodeTcpMuxOpenPayload(payloadInvalidUtf8), /host is not valid UTF-8/i);
});

test("tcp-mux: CLOSE payload encode/decode", () => {
  const payload = encodeTcpMuxClosePayload(TcpMuxCloseFlags.FIN);
  const decoded = decodeTcpMuxClosePayload(payload);
  assert.deepEqual(decoded, { flags: TcpMuxCloseFlags.FIN });
});

test("tcp-mux: ERROR payload encode/decode", () => {
  const payload = encodeTcpMuxErrorPayload(TcpMuxErrorCode.POLICY_DENIED, "nope");
  const decoded = decodeTcpMuxErrorPayload(payload);
  assert.deepEqual(decoded, { code: TcpMuxErrorCode.POLICY_DENIED, message: "nope" });
});

test("tcp-mux: frame parser supports split + concatenated chunks", () => {
  const openFrame = encodeTcpMuxFrame(
    TcpMuxMsgType.OPEN,
    1,
    encodeTcpMuxOpenPayload({ host: "127.0.0.1", port: 7 }),
  );
  const dataFrame = encodeTcpMuxFrame(TcpMuxMsgType.DATA, 1, Buffer.from("hi", "utf8"));

  // Concatenate frames into one byte stream and then split into odd chunks.
  const combined = Buffer.concat([openFrame, dataFrame]);
  const parser = new TcpMuxFrameParser();

  const f0 = parser.push(combined.subarray(0, 3));
  assert.equal(f0.length, 0);
  assert.equal(parser.pendingBytes(), 3);

  const f1 = parser.push(combined.subarray(3, openFrame.length + 1));
  assert.equal(f1.length, 1);
  assert.equal(f1[0].msgType, TcpMuxMsgType.OPEN);
  assert.equal(f1[0].streamId, 1);
  assert.equal(f1[0].payload.length, openFrame.length - TCP_MUX_HEADER_BYTES);

  const f2 = parser.push(combined.subarray(openFrame.length + 1));
  assert.equal(f2.length, 1);
  assert.equal(f2[0].msgType, TcpMuxMsgType.DATA);
  assert.equal(f2[0].streamId, 1);
  assert.equal(f2[0].payload.toString("utf8"), "hi");
  assert.equal(parser.pendingBytes(), 0);
});

test("tcp-mux: frame parser rejects payload length larger than max", () => {
  const header = Buffer.alloc(TCP_MUX_HEADER_BYTES);
  header.writeUInt8(TcpMuxMsgType.DATA, 0);
  header.writeUInt32BE(1, 1);
  header.writeUInt32BE(1024, 5);

  const parser = new TcpMuxFrameParser(16);
  assert.throws(() => parser.push(header), /exceeds max/i);
});

test("tcp-mux: OPEN payload rejects oversized host", () => {
  const hostBytes = Buffer.alloc(MAX_TCP_MUX_OPEN_HOST_BYTES + 1, 0x61);
  const payload = Buffer.allocUnsafe(2 + hostBytes.length + 2 + 2);
  let off = 0;
  payload.writeUInt16BE(hostBytes.length, off);
  off += 2;
  hostBytes.copy(payload, off);
  off += hostBytes.length;
  payload.writeUInt16BE(80, off);
  off += 2;
  payload.writeUInt16BE(0, off); // metadata_len

  assert.throws(() => decodeTcpMuxOpenPayload(payload), /host too long/i);
});

test("tcp-mux: OPEN payload rejects oversized metadata", () => {
  const hostBytes = Buffer.from("a", "utf8");
  const metadataBytes = Buffer.alloc(MAX_TCP_MUX_OPEN_METADATA_BYTES + 1, 0x62);
  const payload = Buffer.allocUnsafe(2 + hostBytes.length + 2 + 2 + metadataBytes.length);
  let off = 0;
  payload.writeUInt16BE(hostBytes.length, off);
  off += 2;
  hostBytes.copy(payload, off);
  off += hostBytes.length;
  payload.writeUInt16BE(80, off);
  off += 2;
  payload.writeUInt16BE(metadataBytes.length, off);
  off += 2;
  metadataBytes.copy(payload, off);

  assert.throws(() => decodeTcpMuxOpenPayload(payload), /metadata too long/i);
});
