import test from "node:test";
import assert from "node:assert/strict";

import { createAdaptiveRingBufferTarget } from "../src/platform/audio.ts";

test("createAdaptiveRingBufferTarget scales underrunCount as missing frames (not events)", () => {
  const target = createAdaptiveRingBufferTarget(1000, 1000, {
    minTargetFrames: 0,
    maxTargetFrames: 1000,
    initialTargetFrames: 100,
    increaseFrames: 10,
    renderQuantumFrames: 128,
    // Disable time-based decrease and low-watermark nudges for deterministic testing.
    stableSeconds: 1e9,
    decreaseIntervalSeconds: 1e9,
    lowWaterMarkRatio: 0,
  });

  // First update seeds the internal lastUnderrun counter.
  assert.equal(target.update(0, 0, 0), 100);

  // Half a render quantum worth of missing frames should only increase the target proportionally:
  // ceil((64 / 128) * 10) == 5
  assert.equal(target.update(0, 64, 1), 105);

  // Another +64 frames missing => +5 again.
  assert.equal(target.update(0, 128, 2), 110);
});

test("createAdaptiveRingBufferTarget handles u32 wrap in underrunCount", () => {
  const target = createAdaptiveRingBufferTarget(1000, 1000, {
    minTargetFrames: 0,
    maxTargetFrames: 1000,
    initialTargetFrames: 100,
    increaseFrames: 10,
    renderQuantumFrames: 128,
    stableSeconds: 1e9,
    decreaseIntervalSeconds: 1e9,
    lowWaterMarkRatio: 0,
  });

  // Seed with a value near u32::MAX without applying a delta.
  assert.equal(target.update(0, 0xffff_fffe, 0), 100);

  // Wrap to 2 implies +4 missing frames.
  // ceil((4 / 128) * 10) == 1
  assert.equal(target.update(0, 2, 1), 101);
});

