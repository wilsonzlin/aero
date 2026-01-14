import { describe, expect, it, vi } from "vitest";

import { RemoteStreamingDisk } from "../platform/remote_disk";
import { OpfsRawDisk } from "./opfs_raw";
import { RuntimeDiskWorker } from "./runtime_disk_worker_impl";
import type { DiskImageMetadata } from "./metadata";
import type { RuntimeDiskRequestMessage, RuntimeDiskResponseMessage } from "./runtime_disk_protocol";

describe("runtime disk open (metadata) prototype pollution hardening", () => {
  it("does not treat local disks as remote-streaming based on inherited Object.prototype.remote", async () => {
    const remoteExisting = Object.getOwnPropertyDescriptor(Object.prototype, "remote");
    if (remoteExisting && remoteExisting.configurable === false) {
      // Extremely unlikely, but avoid breaking the test environment.
      return;
    }

    const streamingSpy = vi.spyOn(RemoteStreamingDisk, "open").mockImplementation(async () => {
      throw new Error("RemoteStreamingDisk.open should not be called");
    });
    const rawSpy = vi.spyOn(OpfsRawDisk, "open").mockImplementation(async () => {
      return {
        sectorSize: 512,
        capacityBytes: 512,
        readSectors: async () => {},
        writeSectors: async () => {},
        flush: async () => {},
        close: async () => {},
      } as unknown as OpfsRawDisk;
    });

    const meta: DiskImageMetadata = {
      source: "local",
      id: "l1",
      name: "Local",
      backend: "opfs",
      kind: "hdd",
      format: "raw",
      fileName: "disk.img",
      sizeBytes: 512,
      createdAtMs: 0,
    };

    const posted: RuntimeDiskResponseMessage[] = [];
    const worker = new RuntimeDiskWorker((msg) => posted.push(msg));

    try {
      Object.defineProperty(Object.prototype, "remote", {
        value: { url: "https://example.com/evil.img" },
        configurable: true,
        writable: true,
      });

      const req = {
        type: "request",
        requestId: 1,
        op: "open",
        payload: { spec: { kind: "local", meta }, mode: "direct" },
      } satisfies RuntimeDiskRequestMessage;
      await worker.handleMessage(req);

      const resp = posted[0];
      expect(resp?.ok).toBe(true);
      expect(streamingSpy).toHaveBeenCalledTimes(0);
      expect(rawSpy).toHaveBeenCalledTimes(1);
    } finally {
      streamingSpy.mockRestore();
      rawSpy.mockRestore();
      if (remoteExisting) Object.defineProperty(Object.prototype, "remote", remoteExisting);
      else Reflect.deleteProperty(Object.prototype, "remote");
    }
  });

  it("ignores inherited Object.prototype.urls when remote.urls is missing", async () => {
    const urlsExisting = Object.getOwnPropertyDescriptor(Object.prototype, "urls");
    if (urlsExisting && urlsExisting.configurable === false) {
      // Extremely unlikely, but avoid breaking the test environment.
      return;
    }

    const openSpy = vi.spyOn(RemoteStreamingDisk, "open").mockImplementation(async () => {
      throw new Error("RemoteStreamingDisk.open should not be called");
    });

    const meta = {
      source: "remote",
      id: "r1",
      name: "Remote",
      kind: "hdd",
      format: "raw",
      sizeBytes: 512,
      createdAtMs: 0,
      remote: {
        imageId: "img",
        version: "v1",
        delivery: "range",
        // urls intentionally missing (simulates corrupt/untrusted metadata)
      },
      cache: {
        chunkSizeBytes: 512,
        backend: "idb",
        fileName: "cache.aerospar",
        overlayFileName: "overlay.aerospar",
        overlayBlockSizeBytes: 512,
      },
    } as unknown as DiskImageMetadata;

    const posted: RuntimeDiskResponseMessage[] = [];
    const worker = new RuntimeDiskWorker((msg) => posted.push(msg));

    try {
      Object.defineProperty(Object.prototype, "urls", {
        value: { url: "https://example.com/evil.img" },
        configurable: true,
        writable: true,
      });

      const req = {
        type: "request",
        requestId: 1,
        op: "open",
        payload: { spec: { kind: "local", meta }, mode: "direct" },
      } satisfies RuntimeDiskRequestMessage;
      await worker.handleMessage(req);

      const resp = posted[0];
      expect(resp).toBeTruthy();
      if (!resp) throw new Error("expected response");
      expect(resp.ok).toBe(false);
      if (resp.ok) throw new Error("expected error response");
      expect(String(resp.error.message)).toMatch(/urls\.url and urls\.leaseEndpoint/i);
      expect(openSpy).toHaveBeenCalledTimes(0);
    } finally {
      openSpy.mockRestore();
      if (urlsExisting) Object.defineProperty(Object.prototype, "urls", urlsExisting);
      else Reflect.deleteProperty(Object.prototype, "urls");
    }
  });
});
