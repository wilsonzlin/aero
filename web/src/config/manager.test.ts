import { describe, expect, it, vi } from "vitest";

import { AeroConfigManager } from "./manager";

describe("AeroConfigManager", () => {
  it("ignores oversized static config responses", async () => {
    const originalFetch = globalThis.fetch;
    globalThis.fetch = vi.fn(async () => {
      return new Response(JSON.stringify({ guestMemoryMiB: 2048 }), {
        status: 200,
        headers: {
          "content-type": "application/json",
          // 1MiB cap + 1
          "content-length": String(1024 * 1024 + 1),
        },
      });
    }) as unknown as typeof fetch;

    try {
      const mgr = new AeroConfigManager({
        staticConfigUrl: "https://example.invalid/config.json",
        capabilities: {
          supportsThreadedWorkers: true,
          threadedWorkersUnsupportedReason: null,
          supportsWebGPU: false,
          webgpuUnsupportedReason: "no",
        },
        queryString: "",
        storage: undefined,
      });

      await expect(mgr.init()).resolves.toBeUndefined();
      // Oversized config should be ignored; defaults apply.
      expect(mgr.getState().effective.guestMemoryMiB).toBe(512);
    } finally {
      globalThis.fetch = originalFetch;
    }
  });
});

