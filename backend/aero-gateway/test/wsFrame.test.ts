import assert from "node:assert/strict";
import test from "node:test";

import { tryReadWsFrame } from "../src/routes/wsFrame.js";

test("tryReadWsFrame treats non-safe 64-bit lengths as protocol error (1002)", () => {
  // fin=1, opcode=2 (binary), mask=0, length=127 (64-bit)
  const buf = Buffer.alloc(2 + 8);
  buf[0] = 0x82;
  buf[1] = 0x7f;
  // Make a 64-bit length that is not a safe JS integer (hi != 0).
  buf.writeUInt32BE(0xffffffff, 2);
  buf.writeUInt32BE(0xffffffff, 6);

  const parsed = tryReadWsFrame(buf, 1024);
  assert.ok(parsed);
  assert.equal(parsed.frame.opcode, 0x8);
  assert.equal(parsed.frame.payload.length, 2);
  // 1002 (protocol error) big-endian.
  assert.deepEqual(parsed.frame.payload, Buffer.from([0x03, 0xea]));
  assert.equal(parsed.remaining.length, 0);
});

