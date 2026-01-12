import { describe, expect, it } from "vitest";

import { getRingBufferLevelFrames, type AudioRingBufferLayout } from "./audio";
import { restoreAudioWorkletRing, type AudioWorkletRingStateLike } from "./audio_ring_restore";
import {
  HEADER_U32_LEN,
  READ_FRAME_INDEX,
  WRITE_FRAME_INDEX,
  requiredBytes,
  wrapRingBuffer,
} from "../audio/audio_worklet_ring";

function createTestRingBuffer(channelCount: number, capacityFrames: number): AudioRingBufferLayout {
  const buffer = new SharedArrayBuffer(requiredBytes(capacityFrames, channelCount));
  const views = wrapRingBuffer(buffer, capacityFrames, channelCount);

  for (let i = 0; i < HEADER_U32_LEN; i++) Atomics.store(views.header, i, 0);

  return {
    buffer,
    ...views,
    channelCount,
    capacityFrames,
  };
}

describe("restoreAudioWorkletRing", () => {
  it("restores indices exactly and clears sample payload to silence", () => {
    const ring = createTestRingBuffer(2, 8);
    ring.samples.fill(123);

    // Seed to non-zero values so we know restore overwrites them.
    Atomics.store(ring.readIndex, 0, 111);
    Atomics.store(ring.writeIndex, 0, 222);

    const state: AudioWorkletRingStateLike = { capacityFrames: 8, readPos: 2, writePos: 5 };
    restoreAudioWorkletRing(ring, state);

    expect(Atomics.load(ring.readIndex, 0)).toBe(2);
    expect(Atomics.load(ring.writeIndex, 0)).toBe(5);
    expect(ring.samples).toEqual(new Float32Array(ring.samples.length));
  });

  it("clamps when writePos-readPos exceeds ring capacity", () => {
    const ring = createTestRingBuffer(1, 8);
    ring.samples.fill(1);

    const state: AudioWorkletRingStateLike = { capacityFrames: 8, readPos: 0, writePos: 100 };
    restoreAudioWorkletRing(ring, state);

    // available = 100, but ring can only hold 8 -> clamp readPos to a consistent full state.
    expect(Atomics.load(ring.readIndex, 0)).toBe(92);
    expect(Atomics.load(ring.writeIndex, 0)).toBe(100);
    expect(getRingBufferLevelFrames(ring)).toBe(8);
    expect(ring.samples).toEqual(new Float32Array(ring.samples.length));
  });

  it("clamps using the actual ring capacity even if snapshot capacityFrames differs", () => {
    const ring = createTestRingBuffer(1, 8);
    ring.samples.fill(1);

    const state: AudioWorkletRingStateLike = { capacityFrames: 16, readPos: 0, writePos: 100 };
    restoreAudioWorkletRing(ring, state);

    // Clamp must use the actual ring capacity (8), not the snapshot's 16.
    expect(Atomics.load(ring.readIndex, 0)).toBe(92);
    expect(Atomics.load(ring.writeIndex, 0)).toBe(100);
    expect(getRingBufferLevelFrames(ring)).toBe(8);
  });

  it("treats snapshot capacityFrames=0 as unknown and still clamps using ring capacity", () => {
    const ring = createTestRingBuffer(1, 8);
    ring.samples.fill(1);

    const state: AudioWorkletRingStateLike = { capacityFrames: 0, readPos: 0, writePos: 100 };
    restoreAudioWorkletRing(ring, state);

    expect(Atomics.load(ring.readIndex, 0)).toBe(92);
    expect(Atomics.load(ring.writeIndex, 0)).toBe(100);
    expect(getRingBufferLevelFrames(ring)).toBe(8);
    expect(ring.samples).toEqual(new Float32Array(ring.samples.length));
  });

  it("treats read/write positions as wrapping u32 counters", () => {
    const ring = createTestRingBuffer(1, 8);
    ring.samples.fill(1);

    const readPos = 0xffff_fffd;
    const writePos = 2;
    const state: AudioWorkletRingStateLike = { capacityFrames: 8, readPos, writePos };
    restoreAudioWorkletRing(ring, state);

    expect(Atomics.load(ring.readIndex, 0)).toBe(readPos);
    expect(Atomics.load(ring.writeIndex, 0)).toBe(writePos);
    expect(getRingBufferLevelFrames(ring)).toBe(5);
  });

  it("clamps correctly when wrapping u32 math yields available > capacity", () => {
    const ring = createTestRingBuffer(1, 8);
    ring.samples.fill(1);

    // Treat read/write as wrapping u32 counters: writePos=1, readPos=0xffff_fff0 -> available=17.
    // Since available > capacity, restore should clamp to a consistent "full" state (available=8).
    const state: AudioWorkletRingStateLike = { capacityFrames: 8, readPos: 0xffff_fff0, writePos: 1 };
    restoreAudioWorkletRing(ring, state);

    expect(Atomics.load(ring.readIndex, 0)).toBe(0xffff_fff9);
    expect(Atomics.load(ring.writeIndex, 0)).toBe(1);
    expect(getRingBufferLevelFrames(ring)).toBe(8);
    expect(ring.samples).toEqual(new Float32Array(ring.samples.length));
  });

  it("does not modify underrun/overrun counters", () => {
    const ring = createTestRingBuffer(1, 8);
    ring.samples.fill(1);

    Atomics.store(ring.underrunCount, 0, 123);
    Atomics.store(ring.overrunCount, 0, 456);

    const state: AudioWorkletRingStateLike = { capacityFrames: 8, readPos: 1, writePos: 2 };
    restoreAudioWorkletRing(ring, state);

    expect(Atomics.load(ring.underrunCount, 0)).toBe(123);
    expect(Atomics.load(ring.overrunCount, 0)).toBe(456);
  });

  it("ignores snapshot capacity mismatches and proceeds", () => {
    const ring = createTestRingBuffer(1, 8);
    ring.samples.fill(1);

    const state: AudioWorkletRingStateLike = { capacityFrames: 4, readPos: 1, writePos: 2 };
    expect(() => restoreAudioWorkletRing(ring, state)).not.toThrow();
    expect(Atomics.load(ring.readIndex, 0)).toBe(1);
    expect(Atomics.load(ring.writeIndex, 0)).toBe(2);
  });
});
