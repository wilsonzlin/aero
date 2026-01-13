import { describe, expect, it, vi } from "vitest";

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
    // Drain the first record so the ring is empty, but the producer tail stays near the end.
    // The next push should require a wrap marker.
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

  it("supports writing records without allocating an intermediate payload buffer", () => {
    const ring = makeRing(64);
    expect(
      ring.tryPushWithWriter(4, (dest) => {
        dest.set(Uint8Array.of(9, 8, 7, 6));
      }),
    ).toBe(true);
    expect(Array.from(ring.tryPop() ?? [])).toEqual([9, 8, 7, 6]);
  });

  it("can consume records without allocating a new payload buffer", () => {
    const ring = makeRing(64);
    expect(ring.tryPush(Uint8Array.of(1, 2, 3))).toBe(true);

    let out: number[] | null = null;
    expect(
      ring.consumeNext((payload) => {
        out = Array.from(payload);
      }),
    ).toBe(true);
    expect(out).toEqual([1, 2, 3]);
    expect(ring.tryPop()).toBeNull();
  });

  it("tryPushWithWriter + consumeNext handle wrap markers", () => {
    const ring = makeRing(32);

    // Write a record that leaves only 4 bytes at the end so the next record must wrap.
    expect(
      ring.tryPushWithWriter(23, (dest) => {
        dest.fill(7);
      }),
    ).toBe(true);

    // Drain the first record so the ring is empty but the producer tail remains near the end.
    let drained: number[] | null = null;
    expect(
      ring.consumeNext((payload) => {
        drained = Array.from(payload);
      }),
    ).toBe(true);
    expect(drained).toEqual(new Array(23).fill(7));

    // This push forces a wrap marker because only 4 bytes remain at the end,
    // which is enough for a marker but not enough for the record.
    expect(
      ring.tryPushWithWriter(4, (dest) => {
        dest.set(Uint8Array.of(1, 2, 3, 4));
      }),
    ).toBe(true);

    let wrapped: number[] | null = null;
    expect(
      ring.consumeNext((payload) => {
        wrapped = Array.from(payload);
      }),
    ).toBe(true);
    expect(wrapped).toEqual([1, 2, 3, 4]);
    expect(ring.tryPop()).toBeNull();
  });

  it("waitForConsumeAsync resolves when data is consumed", async () => {
    const ring = makeRing(64);
    expect(ring.tryPush(Uint8Array.of(1, 2, 3))).toBe(true);

    const waitTask = ring.waitForConsumeAsync(1000);
    // Ensure the awaiter gets a chance to arm before consuming.
    await new Promise<void>((resolve) => setTimeout(resolve, 0));
    expect(ring.tryPop()).not.toBeNull();

    await expect(waitTask).resolves.toBe("ok");
  });

  it("waitForConsumeAsync times out when nothing is consumed", async () => {
    const ring = makeRing(64);
    await expect(ring.waitForConsumeAsync(10)).resolves.toBe("timed-out");
  });

  it("waitForDataAsync polling fallback times out near the deadline", async () => {
    const originalWaitAsync = (Atomics as unknown as { waitAsync?: unknown }).waitAsync;
    (Atomics as unknown as { waitAsync?: unknown }).waitAsync = undefined;
    const timeoutSpy = vi.spyOn(globalThis, "setTimeout");
    try {
      const ring = makeRing(64);

      const start = performance.now();
      await expect(ring.waitForDataAsync(10)).resolves.toBe("timed-out");
      const elapsed = performance.now() - start;

      // We expect to overshoot slightly due to scheduler jitter, but the polling
      // fallback should not burn CPU or drift wildly.
      expect(elapsed).toBeGreaterThanOrEqual(8);
      expect(elapsed).toBeLessThan(100);

      // Optional sanity check: the fallback should not schedule thousands of
      // 0ms timers while waiting.
      expect(timeoutSpy.mock.calls.length).toBeLessThan(50);
    } finally {
      timeoutSpy.mockRestore();
      (Atomics as unknown as { waitAsync?: unknown }).waitAsync = originalWaitAsync;
    }
  });

  it("waitForDataAsync polling fallback resolves when data is pushed", async () => {
    const originalWaitAsync = (Atomics as unknown as { waitAsync?: unknown }).waitAsync;
    (Atomics as unknown as { waitAsync?: unknown }).waitAsync = undefined;
    try {
      const ring = makeRing(64);

      const waitTask = ring.waitForDataAsync(100);
      // Give the waiter a chance to enter its polling loop before pushing.
      await new Promise<void>((resolve) => setTimeout(resolve, 0));
      setTimeout(() => {
        ring.tryPush(Uint8Array.of(1, 2, 3));
      }, 5);

      await expect(waitTask).resolves.toBe("ok");
    } finally {
      (Atomics as unknown as { waitAsync?: unknown }).waitAsync = originalWaitAsync;
    }
  });
});
