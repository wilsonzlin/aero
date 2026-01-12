import { describe, expect, it } from "vitest";

import { getRingBufferOverrunCount, writeRingBufferInterleaved, type AudioRingBufferLayout } from "./audio";
import {
  HEADER_U32_LEN,
  READ_FRAME_INDEX,
  WRITE_FRAME_INDEX,
  framesAvailable,
  framesFree,
  requiredBytes,
  wrapRingBuffer,
} from "../audio/audio_worklet_ring";

function createTestRingBuffer(channelCount: number, capacityFrames: number): AudioRingBufferLayout {
  const buffer = new SharedArrayBuffer(requiredBytes(capacityFrames, channelCount));
  const views = wrapRingBuffer(buffer, capacityFrames, channelCount);

  for (let i = 0; i < HEADER_U32_LEN; i++) {
    Atomics.store(views.header, i, 0);
  }

  return {
    buffer,
    ...views,
    channelCount,
    capacityFrames,
  };
}

describe("writeRingBufferInterleaved overrun telemetry", () => {
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

  it("writes samples correctly across wraparound", () => {
    const ring = createTestRingBuffer(2, 4);

    // Write 3 frames (L0, R0, L1, R1, L2, R2).
    const first = Float32Array.from([0, 1, 2, 3, 4, 5]);
    expect(writeRingBufferInterleaved(ring, first, 48_000, 48_000)).toBe(3);

    // Simulate the consumer draining 2 frames so the next write wraps: 1 frame at the end
    // and 2 frames at the start.
    Atomics.store(ring.header, 0, 2);

    const second = Float32Array.from([100, 101, 102, 103, 104, 105]);
    expect(writeRingBufferInterleaved(ring, second, 48_000, 48_000)).toBe(3);
    expect(getRingBufferOverrunCount(ring)).toBe(0);

    expect(ring.samples).toEqual(Float32Array.from([102, 103, 104, 105, 4, 5, 100, 101]));
  });
});

describe("audio_worklet_ring layout helpers", () => {
  it("computes frame deltas as wrapping u32 values", () => {
    // 1 - 0xffff_fffe == 3 (mod 2^32)
    expect(framesAvailable(0xffff_fffe, 1)).toBe(3);
  });

  it("computes free frames using clamped availability", () => {
    // Producer has written 10 frames but consumer hasn't read any, capacity is 4.
    // Availability clamps to 4, so free is 0.
    expect(framesFree(0, 10, 4)).toBe(0);
  });

  it("wrapRingBuffer provides typed views with expected indexing", () => {
    const sab = new SharedArrayBuffer(requiredBytes(4, 2));
    const ring = wrapRingBuffer(sab, 4, 2);

    Atomics.store(ring.header, READ_FRAME_INDEX, 123);
    Atomics.store(ring.header, WRITE_FRAME_INDEX, 456);
    expect(Atomics.load(ring.readIndex, 0)).toBe(123);
    expect(Atomics.load(ring.writeIndex, 0)).toBe(456);

    expect(ring.samples.length).toBe(8);
  });
});
