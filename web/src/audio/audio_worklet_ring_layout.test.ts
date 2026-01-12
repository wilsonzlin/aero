import { describe, expect, it } from "vitest";

import {
  HEADER_BYTES,
  HEADER_U32_LEN,
  OVERRUN_COUNT_INDEX,
  READ_FRAME_INDEX,
  UNDERRUN_COUNT_INDEX,
  WRITE_FRAME_INDEX,
  framesAvailable,
  framesAvailableClamped,
  framesFree,
  getRingBufferLevelFrames,
  requiredBytes,
  wrapRingBuffer,
} from "./audio_worklet_ring";

describe("audio_worklet_ring SharedArrayBuffer layout", () => {
  it("matches the fixed AudioWorklet playback ABI", () => {
    expect(HEADER_U32_LEN).toBe(4);
    expect(HEADER_BYTES).toBe(16);
    expect(READ_FRAME_INDEX).toBe(0);
    expect(WRITE_FRAME_INDEX).toBe(1);
    expect(UNDERRUN_COUNT_INDEX).toBe(2);
    expect(OVERRUN_COUNT_INDEX).toBe(3);
  });

  it("wrap/clamp helpers use u32 frame counters", () => {
    // 1 - 0xffff_fffe == 3 (mod 2^32)
    expect(framesAvailable(0xffff_fffe, 1)).toBe(3);
    expect(framesAvailableClamped(0, 10, 4)).toBe(4);
    expect(framesFree(0, 2, 4)).toBe(2);
  });

  it("getRingBufferLevelFrames clamps to capacityFrames", () => {
    const sab = new SharedArrayBuffer(requiredBytes(4, 2));
    const views = wrapRingBuffer(sab, 4, 2);
    Atomics.store(views.header, READ_FRAME_INDEX, 0);
    Atomics.store(views.header, WRITE_FRAME_INDEX, 10);
    expect(getRingBufferLevelFrames(views.header, 4)).toBe(4);
  });

  it("wrapRingBuffer exposes header subviews that share memory", () => {
    const sab = new SharedArrayBuffer(requiredBytes(4, 2));
    const views = wrapRingBuffer(sab, 4, 2);

    Atomics.store(views.header, READ_FRAME_INDEX, 123);
    Atomics.store(views.header, WRITE_FRAME_INDEX, 456);

    expect(Atomics.load(views.readIndex, 0)).toBe(123);
    expect(Atomics.load(views.writeIndex, 0)).toBe(456);
    expect(views.samples.length).toBe(8);
  });
});
