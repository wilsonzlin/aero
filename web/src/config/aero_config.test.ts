import { describe, expect, it } from "vitest";
import {
  AERO_GUEST_MEMORY_MAX_MIB,
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
    expect(resolved.effective.virtioNetMode).toBe("modern");
    expect(resolved.effective.virtioInputMode).toBe("modern");
    expect(resolved.effective.virtioSndMode).toBe("modern");
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

    expect(resolved.effective.guestMemoryMiB).toBe(AERO_GUEST_MEMORY_MAX_MIB);
    expect(resolved.effective.logLevel).toBe("info");
  });

  it("validation: proxyUrl accepts http(s) and ws(s) URLs", () => {
    const parsed = parseAeroConfigOverrides({ proxyUrl: "https://example.com" });
    expect(parsed.overrides.proxyUrl).toBe("https://example.com");
    expect(parsed.issues.some((i) => i.key === "proxyUrl")).toBe(false);

    const rel = parseAeroConfigOverrides({ proxyUrl: "/l2" });
    expect(rel.overrides.proxyUrl).toBe("/l2");
    expect(rel.issues.some((i) => i.key === "proxyUrl")).toBe(false);

    const bad = parseAeroConfigOverrides({ proxyUrl: "ftp://example.com" });
    expect(bad.overrides.proxyUrl).toBeNull();
    expect(bad.issues.some((i) => i.key === "proxyUrl")).toBe(true);
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
    expect(parsed.lockedKeys.has("proxyUrl")).toBe(true);
    expect(parsed.overrides.proxyUrl).toBe("https://example.com");
  });

  it("query parsing: accepts virtioNetMode override", () => {
    const parsed = parseAeroConfigQueryOverrides("?virtioNetMode=legacy");
    expect(parsed.lockedKeys.has("virtioNetMode")).toBe(true);
    expect(parsed.overrides.virtioNetMode).toBe("legacy");
  });

  it("query parsing: accepts virtioInputMode override", () => {
    const parsed = parseAeroConfigQueryOverrides("?virtioInputMode=transitional");
    expect(parsed.lockedKeys.has("virtioInputMode")).toBe(true);
    expect(parsed.overrides.virtioInputMode).toBe("transitional");
  });

  it("query parsing: accepts virtioSndMode override", () => {
    const parsed = parseAeroConfigQueryOverrides("?virtioSndMode=legacy");
    expect(parsed.lockedKeys.has("virtioSndMode")).toBe(true);
    expect(parsed.overrides.virtioSndMode).toBe("legacy");
  });

  it("query parsing: accepts same-origin proxy paths", () => {
    const parsed = parseAeroConfigQueryOverrides("?proxy=%2Fl2");
    expect(parsed.lockedKeys.has("proxyUrl")).toBe(true);
    expect(parsed.overrides.proxyUrl).toBe("/l2");
  });

  it("capability detection: does not throw in non-browser environments", () => {
    const caps = detectAeroBrowserCapabilities();
    expect(typeof caps.supportsThreadedWorkers).toBe("boolean");
    expect(typeof caps.supportsWebGPU).toBe("boolean");
  });
});
