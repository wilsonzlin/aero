import assert from "node:assert/strict";
import test from "node:test";

import { tryReadWsFrame } from "../src/routes/wsFrame.js";

test("tryReadWsFrame treats non-safe 64-bit lengths as protocol error (1002)", () => {
  // fin=1, opcode=2 (binary), mask=1, length=127 (64-bit)
  const buf = Buffer.alloc(2 + 8 + 4);
  buf[0] = 0x82;
  buf[1] = 0x80 | 0x7f;
  // Make a 64-bit length that is not a safe JS integer (hi != 0).
  buf.writeUInt32BE(0xffffffff, 2);
  buf.writeUInt32BE(0xffffffff, 6);
  // mask key (ignored in this error case)
  buf.fill(0xaa, 10, 14);

  const parsed = tryReadWsFrame(buf, 1024);
  assert.ok(parsed);
  assert.equal(parsed.frame.opcode, 0x8);
  assert.equal(parsed.frame.payload.length, 2);
  // 1002 (protocol error) big-endian.
  assert.deepEqual(parsed.frame.payload, Buffer.from([0x03, 0xea]));
  assert.equal(parsed.remaining.length, 0);
});

test("tryReadWsFrame rejects unmasked frames as protocol error (1002)", () => {
  // fin=1, opcode=2 (binary), mask=0, len=0
  const buf = Buffer.from([0x82, 0x00]);
  const parsed = tryReadWsFrame(buf, 1024);
  assert.ok(parsed);
  assert.equal(parsed.frame.opcode, 0x8);
  assert.deepEqual(parsed.frame.payload, Buffer.from([0x03, 0xea]));
  assert.equal(parsed.remaining.length, 0);
});

test("tryReadWsFrame rejects non-zero RSV bits as protocol error (1002)", () => {
  // fin=1, rsv1=1, opcode=2, mask=1, len=0, maskKey=4 bytes
  const buf = Buffer.from([0x80 | 0x40 | 0x02, 0x80 | 0x00, 0x01, 0x02, 0x03, 0x04]);
  const parsed = tryReadWsFrame(buf, 1024);
  assert.ok(parsed);
  assert.equal(parsed.frame.opcode, 0x8);
  assert.deepEqual(parsed.frame.payload, Buffer.from([0x03, 0xea]));
  assert.equal(parsed.remaining.length, 0);
});

