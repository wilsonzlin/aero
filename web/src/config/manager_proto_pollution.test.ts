import { describe, expect, it, vi } from "vitest";

vi.mock("./aero_config", async (importOriginal) => {
  const original = await importOriginal<typeof import("./aero_config")>();
  const isRecord = (v: unknown): v is Record<string, unknown> => typeof v === "object" && v !== null;

  return {
    ...original,
    parseAeroConfigOverrides: (input: unknown) => {
      // Simulate a buggy/compromised config parser that returns a `__proto__` override. The config
      // manager must treat this as untrusted and must not allow it to mutate the stored config's
      // prototype chain.
      if (isRecord(input) && (input as { __forceProtoPollution?: unknown }).__forceProtoPollution) {
        return { overrides: JSON.parse('{"__proto__":{"polluted":true}}') as unknown, issues: [] };
      }
      return original.parseAeroConfigOverrides(input);
    },
  };
});

import { AeroConfigManager } from "./manager";

describe("AeroConfigManager prototype pollution hardening", () => {
  it("does not allow __proto__ overrides to mutate storedConfig prototype", () => {
    const manager = new AeroConfigManager({
      capabilities: {
        supportsThreadedWorkers: true,
        threadedWorkersUnsupportedReason: null,
        supportsWebGPU: true,
        webgpuUnsupportedReason: null,
      },
      storage: undefined,
      queryString: "",
    });

    manager.updateStoredConfig(
      { __forceProtoPollution: true } as unknown as Parameters<(typeof AeroConfigManager)["prototype"]["updateStoredConfig"]>[0],
    );

    const storedConfig = (manager as unknown as { storedConfig: Record<string, unknown> }).storedConfig;
    expect(Object.getPrototypeOf(storedConfig)).toBe(null);
    expect(storedConfig.polluted).toBeUndefined();
  });
});
