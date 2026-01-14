import { describe, expect, it, vi } from "vitest";

import { RemoteStreamingDisk } from "../platform/remote_disk";
import { OpfsRawDisk } from "./opfs_raw";
import { RuntimeDiskWorker } from "./runtime_disk_worker_impl";
import type { DiskImageMetadata } from "./metadata";

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
      } as any;
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

    const posted: any[] = [];
    const worker = new RuntimeDiskWorker((msg) => posted.push(msg));

    try {
      Object.defineProperty(Object.prototype, "remote", {
        value: { url: "https://example.com/evil.img" },
        configurable: true,
        writable: true,
      });

      await worker.handleMessage({
        type: "request",
        requestId: 1,
        op: "open",
        payload: { spec: { kind: "local", meta }, mode: "direct" },
      } as any);

      const resp = posted[0];
      expect(resp?.ok).toBe(true);
      expect(streamingSpy).toHaveBeenCalledTimes(0);
      expect(rawSpy).toHaveBeenCalledTimes(1);
    } finally {
      streamingSpy.mockRestore();
      rawSpy.mockRestore();
      if (remoteExisting) Object.defineProperty(Object.prototype, "remote", remoteExisting);
      else delete (Object.prototype as any).remote;
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

    const meta: DiskImageMetadata = {
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
      } as any,
      cache: {
        chunkSizeBytes: 512,
        backend: "idb",
        fileName: "cache.aerospar",
        overlayFileName: "overlay.aerospar",
        overlayBlockSizeBytes: 512,
      },
    };

    const posted: any[] = [];
    const worker = new RuntimeDiskWorker((msg) => posted.push(msg));

    try {
      Object.defineProperty(Object.prototype, "urls", {
        value: { url: "https://example.com/evil.img" },
        configurable: true,
        writable: true,
      });

      await worker.handleMessage({
        type: "request",
        requestId: 1,
        op: "open",
        payload: { spec: { kind: "local", meta }, mode: "direct" },
      } as any);

      const resp = posted[0];
      expect(resp?.ok).toBe(false);
      expect(String(resp?.error?.message ?? "")).toMatch(/urls\.url and urls\.leaseEndpoint/i);
      expect(openSpy).toHaveBeenCalledTimes(0);
    } finally {
      openSpy.mockRestore();
      if (urlsExisting) Object.defineProperty(Object.prototype, "urls", urlsExisting);
      else delete (Object.prototype as any).urls;
    }
  });
});

