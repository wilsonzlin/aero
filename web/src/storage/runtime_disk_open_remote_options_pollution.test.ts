import { describe, expect, it, vi } from "vitest";

import { RemoteStreamingDisk } from "../platform/remote_disk";
import { pickDefaultBackend } from "./metadata";
import { RuntimeDiskWorker } from "./runtime_disk_worker_impl";
import type { RuntimeDiskRequestMessage, RuntimeDiskResponseMessage } from "./runtime_disk_protocol";

describe("runtime disk remote open option pollution", () => {
  it("open(spec=remote) ignores inherited RemoteDiskOpenSpec option fields", async () => {
    const credsExisting = Object.getOwnPropertyDescriptor(Object.prototype, "credentials");
    const backendExisting = Object.getOwnPropertyDescriptor(Object.prototype, "cacheBackend");
    const limitExisting = Object.getOwnPropertyDescriptor(Object.prototype, "cacheLimitBytes");
    if (
      (credsExisting && credsExisting.configurable === false) ||
      (backendExisting && backendExisting.configurable === false) ||
      (limitExisting && limitExisting.configurable === false)
    ) {
      // Extremely unlikely, but avoid breaking the test environment.
      return;
    }

    const openSpy = vi.spyOn(RemoteStreamingDisk, "open").mockResolvedValue({
      sectorSize: 512,
      capacityBytes: 512,
      async readSectors() {},
      async writeSectors() {},
      async flush() {},
    } as unknown as RemoteStreamingDisk);

    const posted: RuntimeDiskResponseMessage[] = [];
    const worker = new RuntimeDiskWorker((msg) => posted.push(msg));

    try {
      const defaultBackend = pickDefaultBackend();
      const pollutedBackend = defaultBackend === "opfs" ? "idb" : "opfs";
      Object.defineProperty(Object.prototype, "credentials", { value: "include", configurable: true, writable: true });
      Object.defineProperty(Object.prototype, "cacheBackend", { value: pollutedBackend, configurable: true, writable: true });
      Object.defineProperty(Object.prototype, "cacheLimitBytes", { value: null, configurable: true, writable: true });

      const req = {
        type: "request",
        requestId: 1,
        op: "open",
        payload: {
          mode: "direct",
          spec: {
            kind: "remote",
            remote: {
              delivery: "range",
              kind: "hdd",
              format: "raw",
              url: "https://example.invalid/disk.img",
              cacheKey: "cache-key",
            },
          },
        },
      } satisfies RuntimeDiskRequestMessage;
      await worker.handleMessage(req);

      expect(openSpy).toHaveBeenCalledTimes(1);
      const options = openSpy.mock.calls[0]![1]!;
      expect(options.credentials).toBe("same-origin");
      expect(options.cacheBackend).toBe(defaultBackend);
      expect(options.cacheLimitBytes).toBeUndefined();

      const resp = posted[0];
      expect(resp.ok).toBe(true);
    } finally {
      openSpy.mockRestore();
      if (credsExisting) Object.defineProperty(Object.prototype, "credentials", credsExisting);
      else Reflect.deleteProperty(Object.prototype, "credentials");
      if (backendExisting) Object.defineProperty(Object.prototype, "cacheBackend", backendExisting);
      else Reflect.deleteProperty(Object.prototype, "cacheBackend");
      if (limitExisting) Object.defineProperty(Object.prototype, "cacheLimitBytes", limitExisting);
      else Reflect.deleteProperty(Object.prototype, "cacheLimitBytes");
    }
  });

  it("openRemote ignores inherited RemoteDiskOptions fields", async () => {
    const backendExisting = Object.getOwnPropertyDescriptor(Object.prototype, "cacheBackend");
    if (backendExisting && backendExisting.configurable === false) {
      // Extremely unlikely, but avoid breaking the test environment.
      return;
    }

    const openSpy = vi.spyOn(RemoteStreamingDisk, "open").mockResolvedValue({
      sectorSize: 512,
      capacityBytes: 512,
      async readSectors() {},
      async writeSectors() {},
      async flush() {},
    } as unknown as RemoteStreamingDisk);

    const posted: RuntimeDiskResponseMessage[] = [];
    const worker = new RuntimeDiskWorker((msg) => posted.push(msg));

    try {
      const defaultBackend = pickDefaultBackend();
      const pollutedBackend = defaultBackend === "opfs" ? "idb" : "opfs";
      Object.defineProperty(Object.prototype, "cacheBackend", { value: pollutedBackend, configurable: true, writable: true });

      const req = {
        type: "request",
        requestId: 1,
        op: "openRemote",
        payload: { url: "https://example.invalid/disk.img", options: {} },
      } satisfies RuntimeDiskRequestMessage;
      await worker.handleMessage(req);

      expect(openSpy).toHaveBeenCalledTimes(1);
      const options = openSpy.mock.calls[0]![1]!;
      expect(options.cacheBackend).toBe(defaultBackend);
    } finally {
      openSpy.mockRestore();
      if (backendExisting) Object.defineProperty(Object.prototype, "cacheBackend", backendExisting);
      else Reflect.deleteProperty(Object.prototype, "cacheBackend");
    }
  });
});
