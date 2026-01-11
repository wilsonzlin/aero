import assert from "node:assert/strict";
import test from "node:test";

import { alignUp } from "../aerogpu/aerogpu_cmd.ts";

test("alignUp works beyond 32-bit signed range and stays aligned", () => {
  // > 2^31, but still within the safe integer range for JS numbers.
  const alreadyAligned = 0x9000_0000;
  assert.equal(alignUp(alreadyAligned, 4), alreadyAligned);

  const unaligned = 0x9000_0001;
  const aligned = alignUp(unaligned, 4);
  assert.equal(aligned, 0x9000_0004);
  assert.equal(aligned % 4, 0);
  assert.ok(aligned >= unaligned);
});

