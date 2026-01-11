import { describe, expect, it } from "vitest";

import { getDefaultRingBufferFrames } from "../../../src/platform/audio";

describe("getDefaultRingBufferFrames", () => {
  it("defaults to ~200ms of audio (sampleRate/5) for common rates", () => {
    // 48kHz → 9,600 frames ≈ 200ms.
    expect(getDefaultRingBufferFrames(48_000)).toBe(9_600);
  });

  it("clamps to a minimum of 2048 frames", () => {
    expect(getDefaultRingBufferFrames(8_000)).toBe(2_048);
  });

  it("stays within the upper bound (<= sampleRate/2)", () => {
    const sr = 192_000;
    const frames = getDefaultRingBufferFrames(sr);
    expect(frames).toBeLessThanOrEqual(Math.floor(sr / 2));
    expect(frames).toBe(38_400);
  });
});

