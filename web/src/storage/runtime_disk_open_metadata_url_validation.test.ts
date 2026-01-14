import { describe, expect, it, vi } from "vitest";

import { RemoteStreamingDisk } from "../platform/remote_disk";
import { RuntimeDiskWorker } from "./runtime_disk_worker_impl";
import type { DiskImageMetadata } from "./metadata";

describe("runtime disk open (metadata) url validation", () => {
  it("ignores inherited remote.urls.url when opening remote disk metadata", async () => {
    const urlExisting = Object.getOwnPropertyDescriptor(Object.prototype, "url");
    if (urlExisting && urlExisting.configurable === false) {
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
        urls: {},
      },
      cache: {
        chunkSizeBytes: 1024,
        backend: "idb",
        fileName: "cache.aerospar",
        overlayFileName: "overlay.aerospar",
        overlayBlockSizeBytes: 1024,
      },
    };

    const posted: any[] = [];
    const worker = new RuntimeDiskWorker((msg) => posted.push(msg));

    try {
      Object.defineProperty(Object.prototype, "url", {
        value: "https://example.com/evil.img",
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
      if (urlExisting) Object.defineProperty(Object.prototype, "url", urlExisting);
      else delete (Object.prototype as any).url;
    }
  });
});
