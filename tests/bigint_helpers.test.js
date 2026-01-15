import test from "node:test";
import assert from "node:assert/strict";

import { asI64, asU64, u64ToNumber } from "../src/workers/bigint.js";

test("bigint helpers: asU64/asI64 roundtrips", () => {
  assert.equal(asU64(-1n), 0xffff_ffff_ffff_ffffn);
  assert.equal(asI64(0xffff_ffff_ffff_ffffn), -1n);
  assert.equal(asI64(0x8000_0000_0000_0000n), -0x8000_0000_0000_0000n);
});

test("bigint helpers: u64ToNumber bounds", () => {
  assert.equal(u64ToNumber(0n), 0);
  assert.equal(u64ToNumber(0xffff_ffffn), 0xffff_ffff);
  assert.throws(() => u64ToNumber(0x1_0000_0000n), RangeError);
  assert.throws(() => u64ToNumber(-1n), RangeError);
});

