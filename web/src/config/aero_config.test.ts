import { describe, expect, it } from "vitest";
import {
  detectAeroBrowserCapabilities,
  getDefaultAeroConfig,
  parseAeroConfigOverrides,
  parseAeroConfigQueryOverrides,
  resolveAeroConfigFromSources,
} from "./aero_config";

describe("AeroConfig", () => {
  it("defaults: basic defaults are applied", () => {
  const caps = {
    supportsThreadedWorkers: true,
    threadedWorkersUnsupportedReason: null,
    supportsWebGPU: false,
    webgpuUnsupportedReason: "no webgpu",
  };

  const resolved = resolveAeroConfigFromSources({ capabilities: caps });
  expect(resolved.effective.guestMemoryMiB).toBe(512);
  expect(resolved.effective.enableWorkers).toBe(true);
  expect(resolved.effective.enableWebGPU).toBe(false);
  expect(resolved.effective.proxyUrl).toBeNull();
  expect(resolved.effective.activeDiskImage).toBeNull();
  expect(resolved.effective.logLevel).toBe("info");
  expect(resolved.effective.uiScale).toBeUndefined();
  });

  it("defaults: enableWorkers defaults off when capabilities do not support it", () => {
  const caps = {
    supportsThreadedWorkers: false,
    threadedWorkersUnsupportedReason: "no sab",
    supportsWebGPU: false,
    webgpuUnsupportedReason: "no webgpu",
  };
  const defaults = getDefaultAeroConfig(caps);
  expect(defaults.enableWorkers).toBe(false);
  });

  it("validation: guestMemoryMiB clamps and invalid logLevel is rejected", () => {
  const caps = {
    supportsThreadedWorkers: true,
    threadedWorkersUnsupportedReason: null,
    supportsWebGPU: true,
    webgpuUnsupportedReason: null,
  };

  const resolved = resolveAeroConfigFromSources({
    capabilities: caps,
    storedConfig: {
      guestMemoryMiB: 99999,
      logLevel: "loud",
    },
  });

  expect(resolved.effective.guestMemoryMiB).toBe(4096);
  expect(resolved.effective.logLevel).toBe("info");
  });

  it("validation: proxyUrl rejects non-ws URLs", () => {
  const parsed = parseAeroConfigOverrides({ proxyUrl: "https://example.com" });
  expect(parsed.overrides.proxyUrl).toBeNull();
  expect(parsed.issues.some((i) => i.key === "proxyUrl")).toBe(true);
  });

  it("querystring overrides: query takes precedence over stored", () => {
  const caps = {
    supportsThreadedWorkers: true,
    threadedWorkersUnsupportedReason: null,
    supportsWebGPU: false,
    webgpuUnsupportedReason: "no webgpu",
  };

  const resolved = resolveAeroConfigFromSources({
    capabilities: caps,
    storedConfig: { guestMemoryMiB: 512, logLevel: "warn" },
    queryString: "?mem=2048&log=debug",
  });

  expect(resolved.effective.guestMemoryMiB).toBe(2048);
  expect(resolved.effective.logLevel).toBe("debug");
  expect(resolved.lockedKeys.has("guestMemoryMiB")).toBe(true);
  expect(resolved.lockedKeys.has("logLevel")).toBe(true);
  });

  it("query parsing: locks only valid overrides", () => {
  const parsed = parseAeroConfigQueryOverrides("?mem=not-a-number&workers=1&proxy=https%3A%2F%2Fexample.com");
  expect(parsed.lockedKeys.has("guestMemoryMiB")).toBe(false);
  expect(parsed.lockedKeys.has("enableWorkers")).toBe(true);
  expect(parsed.lockedKeys.has("proxyUrl")).toBe(false);
  });

  it("capability detection: does not throw in non-browser environments", () => {
  const caps = detectAeroBrowserCapabilities();
  expect(typeof caps.supportsThreadedWorkers).toBe("boolean");
  expect(typeof caps.supportsWebGPU).toBe("boolean");
  });
});
