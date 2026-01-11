import { describe, expect, it } from "vitest";

import { RingBuffer } from "./ring_buffer";
import { Worker } from "node:worker_threads";

function makeRing(capacityBytes: number): RingBuffer {
  const sab = new SharedArrayBuffer(RingBuffer.byteLengthForCapacity(capacityBytes));
  const ring = new RingBuffer(sab, 0, sab.byteLength);
  ring.reset();
  return ring;
}

function bytes(...values: number[]): Uint8Array {
  return Uint8Array.from(values);
}

describe("RingBuffer", () => {
  it("detects empty and preserves FIFO order", () => {
    const ring = makeRing(64);
    expect(ring.pop()).toBeNull();

    expect(ring.push(bytes(1))).toBe(true);
    expect(ring.push(bytes(2, 3))).toBe(true);
    expect(ring.push(bytes(4, 5, 6))).toBe(true);

    expect(Array.from(ring.pop() ?? [])).toEqual([1]);
    expect(Array.from(ring.pop() ?? [])).toEqual([2, 3]);
    expect(Array.from(ring.pop() ?? [])).toEqual([4, 5, 6]);

    expect(ring.pop()).toBeNull();
  });

  it("handles wraparound correctly", () => {
    const ring = makeRing(32);

    // Advance head near the end of the ring.
    const first = Uint8Array.from({ length: 20 }, (_, i) => i);
    expect(ring.push(first)).toBe(true);
    expect(Array.from(ring.pop() ?? [])).toEqual(Array.from(first));

    // This message's payload will wrap around the end of the underlying buffer.
    expect(ring.push(bytes(9, 8, 7, 6, 5))).toBe(true);
    expect(ring.push(bytes(1, 2, 3))).toBe(true);

    expect(Array.from(ring.pop() ?? [])).toEqual([9, 8, 7, 6, 5]);
    expect(Array.from(ring.pop() ?? [])).toEqual([1, 2, 3]);
    expect(ring.pop()).toBeNull();
  });

  it("detects full and can be reused after draining", () => {
    const ring = makeRing(32);

    const pushed: number[] = [];
    for (let i = 0; i < 100; i++) {
      const ok = ring.push(bytes(i));
      if (!ok) break;
      pushed.push(i);
    }

    // Each 1-byte message uses 5 bytes (len u32 + payload). With the one-byte
    // sentinel, a 32-byte ring can hold 6 messages.
    expect(pushed.length).toBe(6);
    expect(ring.push(bytes(255))).toBe(false);

    for (const value of pushed) {
      expect(Array.from(ring.pop() ?? [])).toEqual([value]);
    }
    expect(ring.pop()).toBeNull();

    // Reuse.
    expect(ring.push(bytes(42))).toBe(true);
    expect(Array.from(ring.pop() ?? [])).toEqual([42]);
  });

  it("rejects messages that are too large for the ring", () => {
    const ring = makeRing(32);
    const oversized = new Uint8Array(ring.maxMessageBytes() + 1);
    expect(ring.push(oversized)).toBe(false);
  });

  it("recovers from corrupted length prefixes", () => {
    const ring = makeRing(32);

    // Manually inject a bogus length prefix.
    Atomics.store(ring.meta, 0, 4); // head
    Atomics.store(ring.meta, 1, 0); // tail
    ring.data[0] = 0xff;
    ring.data[1] = 0xff;
    ring.data[2] = 0xff;
    ring.data[3] = 0xff;

    expect(ring.pop()).toBeNull();
    expect(Atomics.load(ring.meta, 0)).toBe(4);
    expect(Atomics.load(ring.meta, 1)).toBe(4);

    // Should remain functional.
    expect(ring.push(bytes(7, 7, 7))).toBe(true);
    expect(Array.from(ring.pop() ?? [])).toEqual([7, 7, 7]);

    // A zero length is also invalid (reserved).
    ring.reset();
    Atomics.store(ring.meta, 0, 4);
    Atomics.store(ring.meta, 1, 0);
    ring.data[0] = 0;
    ring.data[1] = 0;
    ring.data[2] = 0;
    ring.data[3] = 0;
    expect(ring.pop()).toBeNull();
    expect(Atomics.load(ring.meta, 1)).toBe(4);

    // Length looks plausible but extends beyond the currently used bytes; this
    // should also recover by dropping the queue.
    ring.reset();
    // Simulate 8 bytes in the ring (enough for a length prefix plus 4 bytes of
    // payload), but claim a longer payload.
    Atomics.store(ring.meta, 0, 8); // head
    Atomics.store(ring.meta, 1, 0); // tail
    ring.data[0] = 20; // len=20 (<= maxMessageBytes for this ring)
    ring.data[1] = 0;
    ring.data[2] = 0;
    ring.data[3] = 0;
    expect(ring.pop()).toBeNull();
    expect(Atomics.load(ring.meta, 1)).toBe(8);
  });

  it("waitForData returns immediately when data is already available", async () => {
    const ring = makeRing(64);
    expect(ring.push(bytes(1, 2, 3))).toBe(true);

    // If this were to call Atomics.wait while the ring is non-empty, it could
    // block indefinitely on the main thread. The implementation should return
    // immediately.
    expect(await ring.waitForData(0)).toBe("not-equal");
    expect(Array.from(ring.pop() ?? [])).toEqual([1, 2, 3]);
  });

  it("waitForDataAsync is non-blocking and respects timeout", async () => {
    const ring = makeRing(64);
    expect(await ring.waitForDataAsync(0)).toBe("timed-out");

    expect(ring.push(bytes(4, 5))).toBe(true);
    expect(await ring.waitForDataAsync(0)).toBe("not-equal");
    expect(Array.from(ring.pop() ?? [])).toEqual([4, 5]);
  });

  it(
    "transfers messages between threads (SPSC)",
    async () => {
      const capacityBytes = 64;
      const sab = new SharedArrayBuffer(RingBuffer.byteLengthForCapacity(capacityBytes));
      const ring = new RingBuffer(sab, 0, sab.byteLength);
      ring.reset();

    const count = 100;
    const worker = new Worker(new URL("./ring_buffer_consumer_worker.ts", import.meta.url), {
      type: "module",
      workerData: { sab, byteOffset: 0, byteLength: sab.byteLength, count },
      execArgv: ["--experimental-strip-types"],
    });

    try {
      for (let i = 0; i < count; i++) {
        const payload = new Uint8Array(4);
        new DataView(payload.buffer).setUint32(0, i, true);
        while (!ring.push(payload)) {
          await new Promise<void>((resolve) => setTimeout(resolve, 0));
        }
      }

      const received = await new Promise<number[]>((resolve, reject) => {
        worker.once("message", (msg) => resolve(msg as number[]));
        worker.once("error", reject);
        worker.once("exit", (code) => {
          if (code !== 0) reject(new Error(`ring buffer worker exited with code ${code}`));
        });
      });

      expect(received).toEqual(Array.from({ length: count }, (_, i) => i));
      } finally {
        await worker.terminate();
      }
    },
    20_000,
  );
});
