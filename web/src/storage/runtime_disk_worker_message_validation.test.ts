import { describe, expect, it, vi } from "vitest";

import { RuntimeDiskWorker, type OpenDiskFn } from "./runtime_disk_worker_impl";
import type { DiskImageMetadata } from "./metadata";

describe("runtime disk worker message validation", () => {
  it("does not accept top-level fields inherited from Object.prototype", async () => {
    const opExisting = Object.getOwnPropertyDescriptor(Object.prototype, "op");
    if (opExisting && opExisting.configurable === false) {
      // Extremely unlikely, but avoid breaking the test environment.
      return;
    }

    const dummyLocalMeta: DiskImageMetadata = {
      source: "local",
      id: "disk1",
      name: "disk1",
      backend: "idb",
      kind: "hdd",
      format: "raw",
      fileName: "disk1.img",
      sizeBytes: 2 * 1024 * 1024,
      createdAtMs: 0,
    };

    const posted: any[] = [];
    const disk = { sectorSize: 512, capacityBytes: dummyLocalMeta.sizeBytes, async readSectors() {}, async writeSectors() {}, async flush() {} };
    const openDisk: OpenDiskFn = vi.fn(async () => ({ disk, readOnly: false, backendSnapshot: null }));
    const worker = new RuntimeDiskWorker((msg) => posted.push(msg), openDisk);

    try {
      Object.defineProperty(Object.prototype, "op", { value: "open", configurable: true });

      await worker.handleMessage({
        type: "request",
        requestId: 1,
        // Missing `op` must not be satisfied by prototype pollution.
        payload: { spec: { kind: "local", meta: dummyLocalMeta } },
      });

      const resp = posted.shift();
      expect(resp.ok).toBe(false);
      expect(String(resp.error?.message ?? "")).toMatch(/invalid runtime disk op/i);
      expect(openDisk).toHaveBeenCalledTimes(0);
    } finally {
      if (opExisting) Object.defineProperty(Object.prototype, "op", opExisting);
      else Reflect.deleteProperty(Object.prototype, "op");
    }
  });

  it("does not accept open payload fields inherited from Object.prototype", async () => {
    const specExisting = Object.getOwnPropertyDescriptor(Object.prototype, "spec");
    if (specExisting && specExisting.configurable === false) {
      // Extremely unlikely, but avoid breaking the test environment.
      return;
    }

    const dummyLocalMeta: DiskImageMetadata = {
      source: "local",
      id: "disk1",
      name: "disk1",
      backend: "idb",
      kind: "hdd",
      format: "raw",
      fileName: "disk1.img",
      sizeBytes: 2 * 1024 * 1024,
      createdAtMs: 0,
    };

    const posted: any[] = [];
    const disk = { sectorSize: 512, capacityBytes: dummyLocalMeta.sizeBytes, async readSectors() {}, async writeSectors() {}, async flush() {} };
    const openDisk: OpenDiskFn = vi.fn(async () => ({ disk, readOnly: false, backendSnapshot: null }));
    const worker = new RuntimeDiskWorker((msg) => posted.push(msg), openDisk);

    try {
      Object.defineProperty(Object.prototype, "spec", { value: { kind: "local", meta: dummyLocalMeta }, configurable: true });

      await worker.handleMessage({
        type: "request",
        requestId: 1,
        op: "open",
        // Missing `spec` must not be satisfied by prototype pollution.
        payload: {},
      });

      const resp = posted.shift();
      expect(resp.ok).toBe(false);
      expect(String(resp.error?.message ?? "")).toMatch(/open payload/i);
      expect(openDisk).toHaveBeenCalledTimes(0);
    } finally {
      if (specExisting) Object.defineProperty(Object.prototype, "spec", specExisting);
      else Reflect.deleteProperty(Object.prototype, "spec");
    }
  });
});
