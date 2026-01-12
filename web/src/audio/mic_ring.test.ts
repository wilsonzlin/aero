import { describe, expect, it } from "vitest";
import {
  createMicRingBuffer,
  DROPPED_SAMPLES_INDEX,
  micRingBufferReadInto,
  micRingBufferWrite,
  READ_POS_INDEX,
  WRITE_POS_INDEX,
} from "./mic_ring.js";

describe("micRingBufferReadInto", () => {
  it("reads across wrap-around and updates read index", () => {
    const rb = createMicRingBuffer(4);

    // Simulate a wrapped state where the logical sample stream is:
    // [2, 3, 4, 5]
    rb.data.set([4, 5, 2, 3]);
    Atomics.store(rb.header, READ_POS_INDEX, 2);
    Atomics.store(rb.header, WRITE_POS_INDEX, 6);

    const out = new Float32Array(4);
    const read = micRingBufferReadInto(rb, out);

    expect(read).toBe(4);
    expect(Array.from(out)).toEqual([2, 3, 4, 5]);
    expect(Atomics.load(rb.header, READ_POS_INDEX) >>> 0).toBe(6);
  });

  it("write keeps most recent samples of a block when partially dropping", () => {
    const rb = createMicRingBuffer(4);

    expect(micRingBufferWrite(rb, new Float32Array([0, 1, 2]))).toBe(3);
    expect(micRingBufferWrite(rb, new Float32Array([3, 4, 5]))).toBe(1);
    expect(Atomics.load(rb.header, DROPPED_SAMPLES_INDEX) >>> 0).toBe(2);

    const out = new Float32Array(4);
    expect(micRingBufferReadInto(rb, out)).toBe(4);
    expect(Array.from(out)).toEqual([0, 1, 2, 5]);
  });

  it("write+read roundtrip preserves order across wrap-around", () => {
    const rb = createMicRingBuffer(4);
    expect(micRingBufferWrite(rb, new Float32Array([0, 1, 2]))).toBe(3);

    const firstOut = new Float32Array(2);
    expect(micRingBufferReadInto(rb, firstOut)).toBe(2);
    expect(Array.from(firstOut)).toEqual([0, 1]);

    expect(micRingBufferWrite(rb, new Float32Array([3, 4, 5]))).toBe(3);

    const out = new Float32Array(4);
    expect(micRingBufferReadInto(rb, out)).toBe(4);
    expect(Array.from(out)).toEqual([2, 3, 4, 5]);
  });

  it("createMicRingBuffer rejects excessive capacity to avoid huge SharedArrayBuffers", () => {
    // Keep in sync with `web/src/audio/mic_ring.js` and the Rust mic bridge cap.
    const MAX_CAPACITY_SAMPLES = 1_048_576;
    expect(() => createMicRingBuffer(MAX_CAPACITY_SAMPLES + 1)).toThrow(/max/);
  });
});
