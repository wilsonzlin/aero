import { describe, expect, it, vi } from "vitest";

import { AeroConfigManager } from "./manager";
import { AERO_CONFIG_STORAGE_KEY } from "./storage";

class MemoryStorage implements Storage {
  private readonly map = new Map<string, string>();

  get length(): number {
    return this.map.size;
  }

  clear(): void {
    this.map.clear();
  }

  getItem(key: string): string | null {
    return this.map.get(key) ?? null;
  }

  key(index: number): string | null {
    return Array.from(this.map.keys())[index] ?? null;
  }

  removeItem(key: string): void {
    this.map.delete(key);
  }

  setItem(key: string, value: string): void {
    this.map.set(key, value);
  }
}

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
      expect(mgr.getState().effective.vramMiB).toBe(64);
    } finally {
      globalThis.fetch = originalFetch;
    }
  });

  it("scrubs l2TunnelToken secrets from stored config before persisting", () => {
    const storage = new MemoryStorage();
    storage.setItem(
      AERO_CONFIG_STORAGE_KEY,
      JSON.stringify({
        proxyUrl: "https://example.com",
        l2TunnelToken: "sekrit",
        l2TunnelTokenTransport: "query",
      }),
    );

    const mgr = new AeroConfigManager({
      capabilities: {
        supportsThreadedWorkers: true,
        threadedWorkersUnsupportedReason: null,
        supportsWebGPU: false,
        webgpuUnsupportedReason: "no",
      },
      queryString: "",
      storage,
    });

    mgr.updateStoredConfig({ logLevel: "debug" });

    const raw = storage.getItem(AERO_CONFIG_STORAGE_KEY);
    expect(raw).not.toBeNull();
    const parsed = JSON.parse(raw!) as Record<string, unknown>;
    expect(parsed).not.toHaveProperty("l2TunnelToken");
    expect(parsed).not.toHaveProperty("l2TunnelTokenTransport");
    expect(parsed.logLevel).toBe("debug");
    expect(parsed.proxyUrl).toBe("https://example.com");
  });
});
