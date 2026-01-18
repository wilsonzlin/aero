import assert from "node:assert/strict";
import test from "node:test";

import { WsMessageReceiver } from "../src/routes/wsMessage.js";

function encodeWsFrameTest(opcode: number, payload: Buffer, fin: boolean): Buffer {
  const finOpcode = (fin ? 0x80 : 0x00) | (opcode & 0x0f);
  const length = payload.length;
  const maskKey = Buffer.from([0x01, 0x02, 0x03, 0x04]);
  const masked = Buffer.allocUnsafe(payload.length);
  for (let i = 0; i < payload.length; i++) masked[i] = payload[i]! ^ maskKey[i % 4]!;
  if (length < 126) {
    const out = Buffer.allocUnsafe(2 + 4 + length);
    out[0] = finOpcode;
    out[1] = 0x80 | length;
    maskKey.copy(out, 2);
    masked.copy(out, 6);
    return out;
  }
  if (length < 65536) {
    const out = Buffer.allocUnsafe(4 + 4 + length);
    out[0] = finOpcode;
    out[1] = 0x80 | 126;
    out.writeUInt16BE(length, 2);
    maskKey.copy(out, 4);
    masked.copy(out, 8);
    return out;
  }
  throw new Error("test helper only supports payload < 65536");
}

test("WsMessageReceiver reassembles fragmented messages (binary + continuation)", async () => {
  const seen: Array<{ opcode: number; payload: Buffer }> = [];
  let closed = 0;
  let protocolErr = 0;
  let tooLarge = 0;
  const sent: Array<{ opcode: number; payload: Buffer }> = [];

  const receiver = new WsMessageReceiver({
    maxMessageBytes: 1024,
    onMessage: (opcode, payload) => seen.push({ opcode, payload }),
    onClose: () => {
      closed += 1;
    },
    sendWsFrame: (opcode, payload) => sent.push({ opcode, payload }),
    closeWithProtocolError: () => {
      protocolErr += 1;
    },
    closeWithMessageTooLarge: () => {
      tooLarge += 1;
    },
  });

  const first = encodeWsFrameTest(0x2, Buffer.from("hello"), false);
  const second = encodeWsFrameTest(0x0, Buffer.from("world"), true);
  receiver.push(first.subarray(0, 3)); // partial frame: should buffer
  receiver.push(Buffer.concat([first.subarray(3), second]));

  assert.equal(protocolErr, 0);
  assert.equal(tooLarge, 0);
  assert.equal(closed, 0);
  assert.equal(sent.length, 0);
  assert.equal(seen.length, 1);
  assert.equal(seen[0]!.opcode, 0x2);
  assert.equal(seen[0]!.payload.toString("utf8"), "helloworld");
});

test("WsMessageReceiver replies to ping with pong (same payload)", async () => {
  const sent: Array<{ opcode: number; payload: Buffer }> = [];
  const receiver = new WsMessageReceiver({
    maxMessageBytes: 1024,
    onMessage: () => assert.fail("unexpected message"),
    onClose: () => assert.fail("unexpected close"),
    sendWsFrame: (opcode, payload) => sent.push({ opcode, payload }),
    closeWithProtocolError: () => assert.fail("unexpected protocol error"),
    closeWithMessageTooLarge: () => assert.fail("unexpected too-large"),
  });

  receiver.push(encodeWsFrameTest(0x9, Buffer.from([1, 2, 3]), true));
  assert.equal(sent.length, 1);
  assert.equal(sent[0]!.opcode, 0xA);
  assert.deepEqual([...sent[0]!.payload], [1, 2, 3]);
});

test("WsMessageReceiver closes with protocol error on fragmented ping (fin=false)", async () => {
  let protocolErr = 0;
  const receiver = new WsMessageReceiver({
    maxMessageBytes: 1024,
    onMessage: () => assert.fail("unexpected message"),
    onClose: () => assert.fail("unexpected close"),
    sendWsFrame: () => assert.fail("unexpected send"),
    closeWithProtocolError: () => {
      protocolErr += 1;
    },
    closeWithMessageTooLarge: () => assert.fail("unexpected too-large"),
  });

  receiver.push(encodeWsFrameTest(0x9, Buffer.from([1, 2, 3]), false));
  assert.equal(protocolErr, 1);
});

test("WsMessageReceiver closes with protocol error on oversized ping payload (>125 bytes)", async () => {
  let protocolErr = 0;
  const receiver = new WsMessageReceiver({
    maxMessageBytes: 1024,
    onMessage: () => assert.fail("unexpected message"),
    onClose: () => assert.fail("unexpected close"),
    sendWsFrame: () => assert.fail("unexpected send"),
    closeWithProtocolError: () => {
      protocolErr += 1;
    },
    closeWithMessageTooLarge: () => assert.fail("unexpected too-large"),
  });

  receiver.push(encodeWsFrameTest(0x9, Buffer.alloc(126), true));
  assert.equal(protocolErr, 1);
});

test("WsMessageReceiver echoes close frames and invokes onClose", async () => {
  const sent: Array<{ opcode: number; payload: Buffer }> = [];
  let closed = 0;
  const receiver = new WsMessageReceiver({
    maxMessageBytes: 1024,
    onMessage: () => assert.fail("unexpected message"),
    onClose: () => {
      closed += 1;
    },
    sendWsFrame: (opcode, payload) => sent.push({ opcode, payload }),
    closeWithProtocolError: () => assert.fail("unexpected protocol error"),
    closeWithMessageTooLarge: () => assert.fail("unexpected too-large"),
  });

  const payload = Buffer.from([0x03, 0xE8]); // 1000
  receiver.push(encodeWsFrameTest(0x8, payload, true));

  assert.equal(closed, 1);
  assert.equal(sent.length, 1);
  assert.equal(sent[0]!.opcode, 0x8);
  assert.deepEqual([...sent[0]!.payload], [...payload]);
});

test("WsMessageReceiver closes with message-too-large when fragments exceed maxMessageBytes", async () => {
  let tooLarge = 0;
  const receiver = new WsMessageReceiver({
    maxMessageBytes: 5,
    onMessage: () => assert.fail("unexpected message"),
    onClose: () => assert.fail("unexpected close"),
    sendWsFrame: () => assert.fail("unexpected send"),
    closeWithProtocolError: () => assert.fail("unexpected protocol error"),
    closeWithMessageTooLarge: () => {
      tooLarge += 1;
    },
  });

  // First fragment: 4 bytes.
  receiver.push(encodeWsFrameTest(0x2, Buffer.from([0x11, 0x22, 0x33, 0x44]), false));
  // Continuation fragment: +2 bytes => 6 total > 5.
  receiver.push(encodeWsFrameTest(0x0, Buffer.from([0x55, 0x66]), true));

  assert.equal(tooLarge, 1);
});

test("WsMessageReceiver closes with protocol error on unexpected continuation", async () => {
  let protocolErr = 0;
  const receiver = new WsMessageReceiver({
    maxMessageBytes: 1024,
    onMessage: () => assert.fail("unexpected message"),
    onClose: () => assert.fail("unexpected close"),
    sendWsFrame: () => assert.fail("unexpected send"),
    closeWithProtocolError: () => {
      protocolErr += 1;
    },
    closeWithMessageTooLarge: () => assert.fail("unexpected too-large"),
  });

  receiver.push(encodeWsFrameTest(0x0, Buffer.alloc(0), true)); // continuation without start
  assert.equal(protocolErr, 1);
});

