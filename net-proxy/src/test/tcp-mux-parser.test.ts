import test from "node:test";
import assert from "node:assert/strict";
import { TCP_MUX_HEADER_BYTES, TcpMuxFrameParser, TcpMuxMsgType, encodeTcpMuxFrame } from "../tcpMuxProtocol";

test("TcpMuxFrameParser parses fragmented frames across pushes", () => {
  const parser = new TcpMuxFrameParser(1024);
  const payload = Buffer.from("hello");
  const frame = encodeTcpMuxFrame(TcpMuxMsgType.DATA, 123, payload);

  const partA = frame.subarray(0, 3);
  const partB = frame.subarray(3);

  assert.deepEqual(parser.push(partA), []);

  const frames = parser.push(partB);
  assert.equal(frames.length, 1);
  assert.equal(frames[0]!.msgType, TcpMuxMsgType.DATA);
  assert.equal(frames[0]!.streamId, 123);
  assert.deepEqual(frames[0]!.payload, payload);
  assert.equal(parser.pendingBytes(), 0);
});

test("TcpMuxFrameParser parses multiple frames from a single chunk", () => {
  const parser = new TcpMuxFrameParser(1024);
  const f1 = encodeTcpMuxFrame(TcpMuxMsgType.PING, 0, Buffer.from([1, 2, 3]));
  const f2 = encodeTcpMuxFrame(TcpMuxMsgType.PONG, 0, Buffer.from([4, 5]));

  const frames = parser.push(Buffer.concat([f1, f2]));
  assert.equal(frames.length, 2);
  assert.equal(frames[0]!.msgType, TcpMuxMsgType.PING);
  assert.deepEqual(frames[0]!.payload, Buffer.from([1, 2, 3]));
  assert.equal(frames[1]!.msgType, TcpMuxMsgType.PONG);
  assert.deepEqual(frames[1]!.payload, Buffer.from([4, 5]));
  assert.equal(parser.pendingBytes(), 0);
});

test("TcpMuxFrameParser rejects frames with oversized payload length", () => {
  const parser = new TcpMuxFrameParser(4);

  const header = Buffer.alloc(TCP_MUX_HEADER_BYTES);
  header.writeUInt8(TcpMuxMsgType.DATA, 0);
  header.writeUInt32BE(1, 1);
  header.writeUInt32BE(5, 5);

  assert.throws(() => parser.push(header), /exceeds max/);
});

