import { describe, expect, it } from "vitest";
import { createMicRingBuffer, micRingBufferReadInto, READ_POS_INDEX, WRITE_POS_INDEX } from "./mic_ring";

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
});

