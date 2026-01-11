import { describe, expect, it } from "vitest";

import { RingBuffer } from "../ipc/ring_buffer";
import { RECORD_ALIGN, ringCtrl } from "../ipc/layout";
import { Worker } from "node:worker_threads";
import {
  COMMAND_RING_CAPACITY_BYTES,
  CONTROL_BYTES,
  EVENT_RING_CAPACITY_BYTES,
  RUNTIME_RESERVED_BYTES,
  STATUS_BYTES,
  WORKER_ROLES,
  allocateSharedMemorySegments,
  createSharedMemoryViews,
  ringRegionsForWorker,
} from "./shared_layout";

describe("runtime/shared_layout", () => {
  // Shared-memory layout includes a demo shared framebuffer region embedded in
  // the guest `WebAssembly.Memory`. Allocate enough guest RAM to fit that region
  // (1 MiB is too small once the runtime reserved bytes are accounted for).
  const TEST_GUEST_RAM_MIB = 5;

  it("places status + rings without overlap", () => {
    const regions: Array<{ name: string; start: number; end: number }> = [
      { name: "status", start: 0, end: STATUS_BYTES },
    ];

    const expectedCommandBytes = ringCtrl.BYTES + COMMAND_RING_CAPACITY_BYTES;
    const expectedEventBytes = ringCtrl.BYTES + EVENT_RING_CAPACITY_BYTES;

    for (const role of WORKER_ROLES) {
      const r = ringRegionsForWorker(role);
      expect(r.command.byteLength).toBe(expectedCommandBytes);
      expect(r.event.byteLength).toBe(expectedEventBytes);

      regions.push({
        name: `${role}.command`,
        start: r.command.byteOffset,
        end: r.command.byteOffset + r.command.byteLength,
      });
      regions.push({
        name: `${role}.event`,
        start: r.event.byteOffset,
        end: r.event.byteOffset + r.event.byteLength,
      });
    }

    for (const region of regions) {
      expect(region.start).toBeGreaterThanOrEqual(0);
      expect(region.end).toBeGreaterThan(region.start);
      expect(region.end).toBeLessThanOrEqual(CONTROL_BYTES);
      expect(region.start % 4).toBe(0);
    }

    const sorted = regions.slice().sort((a, b) => a.start - b.start);
    for (let i = 1; i < sorted.length; i++) {
      const prev = sorted[i - 1];
      const cur = sorted[i];
      expect(cur.start).toBeGreaterThanOrEqual(prev.end);
    }
  });

  it("initializes ring headers using the AIPC layout", () => {
    expect(COMMAND_RING_CAPACITY_BYTES % RECORD_ALIGN).toBe(0);
    expect(EVENT_RING_CAPACITY_BYTES % RECORD_ALIGN).toBe(0);

    const segments = allocateSharedMemorySegments({ guestRamMiB: TEST_GUEST_RAM_MIB });
    for (const role of WORKER_ROLES) {
      const regions = ringRegionsForWorker(role);

      const cmdCtrl = new Int32Array(segments.control, regions.command.byteOffset, ringCtrl.WORDS);
      expect(Array.from(cmdCtrl)).toEqual([0, 0, 0, COMMAND_RING_CAPACITY_BYTES]);

      const evtCtrl = new Int32Array(segments.control, regions.event.byteOffset, ringCtrl.WORDS);
      expect(Array.from(evtCtrl)).toEqual([0, 0, 0, EVENT_RING_CAPACITY_BYTES]);
    }
  });

  it("transfers messages across threads using a shared_layout ring", async () => {
    const segments = allocateSharedMemorySegments({ guestRamMiB: TEST_GUEST_RAM_MIB });
    const regions = ringRegionsForWorker("cpu");
    const ring = new RingBuffer(segments.control, regions.command.byteOffset);

    const count = 100;
    const worker = new Worker(new URL("./shared_layout_ring_consumer_worker.ts", import.meta.url), {
      type: "module",
      workerData: { sab: segments.control, offsetBytes: regions.command.byteOffset, count },
      execArgv: ["--experimental-strip-types"],
    });

    try {
      for (let i = 0; i < count; i++) {
        const payload = new Uint8Array(4);
        new DataView(payload.buffer).setUint32(0, i, true);
        while (!ring.tryPush(payload)) {
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
  });

  it("creates shared views for control + guest memory", () => {
    const segments = allocateSharedMemorySegments({ guestRamMiB: TEST_GUEST_RAM_MIB });
    const views = createSharedMemoryViews(segments);

    expect(views.status.byteOffset).toBe(0);
    expect(views.status.byteLength).toBe(STATUS_BYTES);
    expect(views.guestLayout.guest_base).toBe(RUNTIME_RESERVED_BYTES);
    expect(views.guestLayout.guest_size).toBe(TEST_GUEST_RAM_MIB * 1024 * 1024);
    expect(views.guestU8.byteOffset).toBe(views.guestLayout.guest_base);
    expect(views.guestU8.byteLength).toBe(views.guestLayout.guest_size);
    expect(views.guestU8.buffer).toBe(segments.guestMemory.buffer);
    expect(views.sharedFramebuffer).toBe(segments.sharedFramebuffer);
    expect(views.sharedFramebufferOffsetBytes).toBe(segments.sharedFramebufferOffsetBytes);
  });
});
