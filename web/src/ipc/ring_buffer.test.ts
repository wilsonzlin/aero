import { describe, expect, it } from "vitest";

import { ringCtrl } from "./layout";
import { RingBuffer } from "./ring_buffer";

function makeRing(capacityBytes: number): RingBuffer {
  const sab = new SharedArrayBuffer(ringCtrl.BYTES + capacityBytes);
  new Int32Array(sab, 0, ringCtrl.WORDS).set([0, 0, 0, capacityBytes]);
  return new RingBuffer(sab, 0);
}

describe("ipc/ring_buffer", () => {
  it("preserves FIFO order", () => {
    const ring = makeRing(64);
    expect(ring.tryPop()).toBeNull();

    expect(ring.tryPush(Uint8Array.of(1))).toBe(true);
    expect(ring.tryPush(Uint8Array.of(2, 3))).toBe(true);
    expect(ring.tryPush(Uint8Array.of(4, 5, 6))).toBe(true);

    expect(Array.from(ring.tryPop() ?? [])).toEqual([1]);
    expect(Array.from(ring.tryPop() ?? [])).toEqual([2, 3]);
    expect(Array.from(ring.tryPop() ?? [])).toEqual([4, 5, 6]);
    expect(ring.tryPop()).toBeNull();
  });

  it("writes a wrap marker when a record would cross the end", () => {
    const ring = makeRing(32);

    const first = new Uint8Array(23);
    first.fill(7);
    expect(ring.tryPush(first)).toBe(true);

    // Advance the head so there is free space at the start of the ring, while leaving the tail near the end.
    expect(Array.from(ring.tryPop() ?? [])).toEqual(Array.from(first));

    // This push forces a wrap marker because only 4 bytes remain at the end,
    // which is enough for a marker but not enough for the record.
    expect(ring.tryPush(Uint8Array.of(1, 2, 3, 4))).toBe(true);

    expect(Array.from(ring.tryPop() ?? [])).toEqual([1, 2, 3, 4]);
    expect(ring.tryPop()).toBeNull();
  });

  it("detects full and can be reused after draining", () => {
    const ring = makeRing(32);
    let pushed = 0;
    while (ring.tryPush(Uint8Array.of(pushed))) pushed++;

    // Each 1-byte message consumes 8 bytes (len u32 + payload + padding).
    expect(pushed).toBe(4);
    expect(ring.tryPush(Uint8Array.of(99))).toBe(false);

    for (let i = 0; i < pushed; i++) {
      expect(Array.from(ring.tryPop() ?? [])).toEqual([i]);
    }
    expect(ring.tryPop()).toBeNull();

    expect(ring.tryPush(Uint8Array.of(42))).toBe(true);
    expect(Array.from(ring.tryPop() ?? [])).toEqual([42]);
  });

  it("supports zero-length payloads", () => {
    const ring = makeRing(16);
    expect(ring.tryPush(new Uint8Array(0))).toBe(true);
    expect((ring.tryPop() ?? new Uint8Array(1)).byteLength).toBe(0);
  });
});
