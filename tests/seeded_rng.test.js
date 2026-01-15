import test from "node:test";
import assert from "node:assert/strict";

import { createRandomSource, deriveSeed, randomAlignedOffset, randomInt } from "../web/src/bench/seeded_rng.js";

test("deriveSeed produces stable, distinct stream seeds", () => {
  assert.equal(deriveSeed(1234, 1), deriveSeed(1234, 1));
  assert.notEqual(deriveSeed(1234, 1), deriveSeed(1234, 2));
});

test("randomAlignedOffset is deterministic with the same seed+stream", () => {
  const seed = 1337;
  const stream = 42;
  const maxBytes = 1024 * 1024;
  const blockBytes = 4096;

  const randA = createRandomSource(seed, stream);
  const randB = createRandomSource(seed, stream);

  const seqA = [];
  const seqB = [];
  for (let i = 0; i < 50; i++) {
    seqA.push(randomAlignedOffset(maxBytes, blockBytes, randA));
    seqB.push(randomAlignedOffset(maxBytes, blockBytes, randB));
  }

  assert.deepEqual(seqA, seqB);
  for (const off of seqA) {
    assert.equal(off % blockBytes, 0);
    assert.ok(off >= 0 && off <= maxBytes - blockBytes);
  }
});

test("seeded_rng guards against invalid inputs and out-of-range RandomSource output", () => {
  assert.equal(randomInt(NaN, () => 0.5), 0);
  assert.equal(randomInt(Infinity, () => 0.5), 0);
  assert.equal(randomInt(10, () => NaN), 0);
  assert.equal(randomInt(10, () => -1), 0);
  assert.equal(randomInt(10, () => 1), 9);

  assert.equal(randomAlignedOffset(NaN, 4096, () => 0.5), 0);
  assert.equal(randomAlignedOffset(1024, NaN, () => 0.5), 0);
  assert.equal(randomAlignedOffset(Infinity, 4096, () => 0.5), 0);
  assert.equal(randomAlignedOffset(10_000, 4096, () => 1), 4096);
});
