import { describe, expect, it } from "vitest";

import {
  getRingBufferOverrunCount,
  writeRingBufferInterleaved,
  type AudioRingBufferLayout,
} from "../../../src/platform/audio";

function createTestRingBuffer(channelCount: number, capacityFrames: number): AudioRingBufferLayout {
  const headerU32Len = 4;
  const headerBytes = headerU32Len * Uint32Array.BYTES_PER_ELEMENT;
  const sampleCapacity = capacityFrames * channelCount;
  const buffer = new SharedArrayBuffer(headerBytes + sampleCapacity * Float32Array.BYTES_PER_ELEMENT);

  const header = new Uint32Array(buffer, 0, headerU32Len);
  const samples = new Float32Array(buffer, headerBytes, sampleCapacity);

  for (let i = 0; i < headerU32Len; i++) {
    Atomics.store(header, i, 0);
  }

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

describe("audio ring buffer overruns", () => {
  it("increments overrunCount when write is truncated", () => {
    const ring = createTestRingBuffer(2, 4);

    // Fill 3/4 frames.
    const first = new Float32Array(3 * 2).fill(1);
    expect(writeRingBufferInterleaved(ring, first, 48_000, 48_000)).toBe(3);
    expect(getRingBufferOverrunCount(ring)).toBe(0);

    // Attempt to write 2 more frames; only 1 is free -> 1 dropped frame.
    const second = new Float32Array(2 * 2).fill(2);
    expect(writeRingBufferInterleaved(ring, second, 48_000, 48_000)).toBe(1);
    expect(getRingBufferOverrunCount(ring)).toBe(1);
  });

  it("does not increment overrunCount when fully written", () => {
    const ring = createTestRingBuffer(2, 4);

    const buf = new Float32Array(2 * 2).fill(1);
    expect(writeRingBufferInterleaved(ring, buf, 48_000, 48_000)).toBe(2);
    expect(getRingBufferOverrunCount(ring)).toBe(0);
  });

  it("counts dropped frames even when no frames can be written", () => {
    const ring = createTestRingBuffer(2, 4);

    // Fill the buffer completely.
    const fill = new Float32Array(4 * 2).fill(1);
    expect(writeRingBufferInterleaved(ring, fill, 48_000, 48_000)).toBe(4);
    expect(getRingBufferOverrunCount(ring)).toBe(0);

    // Now the buffer is full; the whole write is dropped.
    const dropped = new Float32Array(3 * 2).fill(2);
    expect(writeRingBufferInterleaved(ring, dropped, 48_000, 48_000)).toBe(0);
    expect(getRingBufferOverrunCount(ring)).toBe(3);
  });

  it("wraps overrunCount as u32", () => {
    const ring = createTestRingBuffer(1, 1);

    // Seed the counter near u32::MAX.
    Atomics.store(ring.header, 3, 0xffff_fffe);

    // Fill the ring buffer so the next write is fully dropped.
    expect(writeRingBufferInterleaved(ring, new Float32Array([0]), 48_000, 48_000)).toBe(1);

    // Drop 4 frames -> 0xffff_fffe + 4 == 2 (mod 2^32).
    expect(writeRingBufferInterleaved(ring, new Float32Array(4), 48_000, 48_000)).toBe(0);
    expect(getRingBufferOverrunCount(ring)).toBe(2);
  });
});
