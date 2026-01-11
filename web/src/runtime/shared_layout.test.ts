import { describe, expect, it } from "vitest";

import { RingBuffer } from "./ring_buffer";
import {
  COMMAND_RING_CAPACITY_BYTES,
  CONTROL_BYTES,
  EVENT_RING_CAPACITY_BYTES,
  STATUS_BYTES,
  WORKER_ROLES,
  createSharedMemoryViews,
  ringRegionsForWorker,
} from "./shared_layout";

describe("runtime/shared_layout", () => {
  it("places status + rings without overlap", () => {
    const regions: Array<{ name: string; start: number; end: number }> = [
      { name: "status", start: 0, end: STATUS_BYTES },
    ];

    const expectedCommandBytes = RingBuffer.byteLengthForCapacity(COMMAND_RING_CAPACITY_BYTES);
    const expectedEventBytes = RingBuffer.byteLengthForCapacity(EVENT_RING_CAPACITY_BYTES);

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

  it("creates shared views for control + guest memory", () => {
    const control = new SharedArrayBuffer(CONTROL_BYTES);
    const guestMemory = new WebAssembly.Memory({ initial: 1, maximum: 1, shared: true });
    const vgaFramebuffer = new SharedArrayBuffer(4);
    const ioIpc = new SharedArrayBuffer(0);
    const views = createSharedMemoryViews({ control, guestMemory, vgaFramebuffer, ioIpc });

    expect(views.status.byteOffset).toBe(0);
    expect(views.status.byteLength).toBe(STATUS_BYTES);
    expect(views.guestU8.byteLength).toBe(64 * 1024);
    expect(views.guestU8.buffer).toBe(guestMemory.buffer);
  });
});
