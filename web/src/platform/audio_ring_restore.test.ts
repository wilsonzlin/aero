import { describe, expect, it } from "vitest";

import { getRingBufferLevelFrames, type AudioRingBufferLayout } from "./audio";
import { restoreAudioWorkletRing, type AudioWorkletRingStateLike } from "./audio_ring_restore";

function createTestRingBuffer(channelCount: number, capacityFrames: number): AudioRingBufferLayout {
  const headerU32Len = 4;
  const headerBytes = headerU32Len * Uint32Array.BYTES_PER_ELEMENT;
  const sampleCapacity = capacityFrames * channelCount;
  const buffer = new SharedArrayBuffer(headerBytes + sampleCapacity * Float32Array.BYTES_PER_ELEMENT);

  const header = new Uint32Array(buffer, 0, headerU32Len);
  const samples = new Float32Array(buffer, headerBytes, sampleCapacity);

  for (let i = 0; i < headerU32Len; i++) Atomics.store(header, i, 0);

  return {
    buffer,
    header,
    readIndex: header.subarray(0, 1),
    writeIndex: header.subarray(1, 2),
    underrunCount: header.subarray(2, 3),
    overrunCount: header.subarray(3, 4),
    samples,
    channelCount,
    capacityFrames,
  };
}

describe("restoreAudioWorkletRing", () => {
  it("restores indices exactly and clears sample payload to silence", () => {
    const ring = createTestRingBuffer(2, 8);
    ring.samples.fill(123);

    // Seed to non-zero values so we know restore overwrites them.
    Atomics.store(ring.header, 0, 111);
    Atomics.store(ring.header, 1, 222);

    const state: AudioWorkletRingStateLike = { capacityFrames: 8, readPos: 2, writePos: 5 };
    restoreAudioWorkletRing(ring, state);

    expect(Atomics.load(ring.header, 0)).toBe(2);
    expect(Atomics.load(ring.header, 1)).toBe(5);
    expect(ring.samples).toEqual(new Float32Array(ring.samples.length));
  });

  it("clamps when writePos-readPos exceeds ring capacity", () => {
    const ring = createTestRingBuffer(1, 8);
    ring.samples.fill(1);

    const state: AudioWorkletRingStateLike = { capacityFrames: 8, readPos: 0, writePos: 100 };
    restoreAudioWorkletRing(ring, state);

    // available = 100, but ring can only hold 8 -> clamp readPos to a consistent full state.
    expect(Atomics.load(ring.header, 0)).toBe(92);
    expect(Atomics.load(ring.header, 1)).toBe(100);
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

    expect(Atomics.load(ring.header, 0)).toBe(readPos);
    expect(Atomics.load(ring.header, 1)).toBe(writePos);
    expect(getRingBufferLevelFrames(ring)).toBe(5);
  });

  it("ignores snapshot capacity mismatches and proceeds", () => {
    const ring = createTestRingBuffer(1, 8);
    ring.samples.fill(1);

    const state: AudioWorkletRingStateLike = { capacityFrames: 4, readPos: 1, writePos: 2 };
    expect(() => restoreAudioWorkletRing(ring, state)).not.toThrow();
    expect(Atomics.load(ring.header, 0)).toBe(1);
    expect(Atomics.load(ring.header, 1)).toBe(2);
  });
});

