import { describe, expect, it } from "vitest";
import {
  AERO_GUEST_MEMORY_MAX_MIB,
  AERO_VRAM_MAX_MIB,
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
    expect(resolved.effective.vramMiB).toBe(64);
    expect(resolved.effective.enableWorkers).toBe(true);
    expect(resolved.effective.enableWebGPU).toBe(false);
    expect(resolved.effective.proxyUrl).toBeNull();
    expect(resolved.effective.activeDiskImage).toBeNull();
    expect(resolved.effective.logLevel).toBe("info");
    expect(resolved.effective.uiScale).toBeUndefined();
    expect(resolved.effective.l2TunnelTransport).toBe("ws");
    expect(resolved.effective.l2RelaySignalingMode).toBe("ws-trickle");
    expect(resolved.effective.l2TunnelToken).toBeUndefined();
    expect(resolved.effective.l2TunnelTokenTransport).toBe("query");
    expect(resolved.effective.vmRuntime).toBe("legacy");
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
        vramMiB: 99999,
        logLevel: "loud",
      },
    });

    expect(resolved.effective.guestMemoryMiB).toBe(AERO_GUEST_MEMORY_MAX_MIB);
    expect(resolved.effective.vramMiB).toBe(AERO_VRAM_MAX_MIB);
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
      storedConfig: { guestMemoryMiB: 512, vramMiB: 16, logLevel: "warn" },
      queryString: "?mem=2048&vram=32&log=debug",
    });

    expect(resolved.effective.guestMemoryMiB).toBe(2048);
    expect(resolved.effective.vramMiB).toBe(32);
    expect(resolved.effective.logLevel).toBe("debug");
    expect(resolved.lockedKeys.has("guestMemoryMiB")).toBe(true);
    expect(resolved.lockedKeys.has("vramMiB")).toBe(true);
    expect(resolved.lockedKeys.has("logLevel")).toBe(true);
  });

  it("query parsing: locks only valid overrides", () => {
    const parsed = parseAeroConfigQueryOverrides("?mem=not-a-number&vram=not-a-number&workers=1&proxy=https%3A%2F%2Fexample.com");
    expect(parsed.lockedKeys.has("guestMemoryMiB")).toBe(false);
    expect(parsed.lockedKeys.has("vramMiB")).toBe(false);
    expect(parsed.lockedKeys.has("enableWorkers")).toBe(true);
    expect(parsed.lockedKeys.has("proxyUrl")).toBe(true);
    expect(parsed.overrides.proxyUrl).toBe("https://example.com");
  });

  it("query parsing: accepts L2 tunnel overrides", () => {
    const parsed = parseAeroConfigQueryOverrides(
      "?l2=webrtc&l2Signal=http-offer&l2Token=sekrit&l2TokenTransport=subprotocol",
    );
    expect(parsed.overrides.l2TunnelTransport).toBe("webrtc");
    expect(parsed.overrides.l2RelaySignalingMode).toBe("http-offer");
    expect(parsed.overrides.l2TunnelToken).toBe("sekrit");
    expect(parsed.overrides.l2TunnelTokenTransport).toBe("subprotocol");
    expect(parsed.lockedKeys.has("l2TunnelTransport")).toBe(true);
    expect(parsed.lockedKeys.has("l2RelaySignalingMode")).toBe(true);
    expect(parsed.lockedKeys.has("l2TunnelToken")).toBe(true);
    expect(parsed.lockedKeys.has("l2TunnelTokenTransport")).toBe(true);
  });

  it("query parsing: ignores invalid L2 tunnel overrides (and does not lock)", () => {
    const parsed = parseAeroConfigQueryOverrides(
      "?l2=bogus&l2Signal=bogus&l2Token=&l2TokenTransport=bogus",
    );
    expect(parsed.overrides.l2TunnelTransport).toBeUndefined();
    expect(parsed.overrides.l2RelaySignalingMode).toBeUndefined();
    expect(parsed.overrides.l2TunnelToken).toBeUndefined();
    expect(parsed.overrides.l2TunnelTokenTransport).toBeUndefined();
    expect(parsed.lockedKeys.has("l2TunnelTransport")).toBe(false);
    expect(parsed.lockedKeys.has("l2RelaySignalingMode")).toBe(false);
    expect(parsed.lockedKeys.has("l2TunnelToken")).toBe(false);
    expect(parsed.lockedKeys.has("l2TunnelTokenTransport")).toBe(false);
  });

  it("querystring overrides: L2 query takes precedence over stored", () => {
    const caps = {
      supportsThreadedWorkers: true,
      threadedWorkersUnsupportedReason: null,
      supportsWebGPU: false,
      webgpuUnsupportedReason: "no webgpu",
    };

    const resolved = resolveAeroConfigFromSources({
      capabilities: caps,
      storedConfig: { l2TunnelTransport: "webrtc", l2RelaySignalingMode: "legacy-offer" },
      queryString: "?l2=ws&l2Signal=http-offer",
    });

    // Stored layer should apply to requested config...
    expect(resolved.requested.l2TunnelTransport).toBe("webrtc");
    expect(resolved.requested.l2RelaySignalingMode).toBe("legacy-offer");
    // ...but query overrides win for effective config.
    expect(resolved.effective.l2TunnelTransport).toBe("ws");
    expect(resolved.effective.l2RelaySignalingMode).toBe("http-offer");
    expect(resolved.lockedKeys.has("l2TunnelTransport")).toBe(true);
    expect(resolved.lockedKeys.has("l2RelaySignalingMode")).toBe(true);
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

  it("query parsing: accepts input backend overrides", () => {
    const parsed = parseAeroConfigQueryOverrides("?kbd=ps2&mouse=virtio");
    expect(parsed.lockedKeys.has("forceKeyboardBackend")).toBe(true);
    expect(parsed.lockedKeys.has("forceMouseBackend")).toBe(true);
    expect(parsed.overrides.forceKeyboardBackend).toBe("ps2");
    expect(parsed.overrides.forceMouseBackend).toBe("virtio");
  });

  it("query parsing: rejects invalid input backend overrides", () => {
    const parsed = parseAeroConfigQueryOverrides("?kbd=wat");
    expect(parsed.lockedKeys.has("forceKeyboardBackend")).toBe(false);
    expect(parsed.overrides.forceKeyboardBackend).toBeUndefined();
    expect(parsed.issues.some((i) => i.key === "forceKeyboardBackend")).toBe(true);
  });

  it("query parsing: accepts vm runtime override", () => {
    const parsed = parseAeroConfigQueryOverrides("?vm=machine");
    expect(parsed.lockedKeys.has("vmRuntime")).toBe(true);
    expect(parsed.overrides.vmRuntime).toBe("machine");
  });

  it("query parsing: accepts vmRuntime override", () => {
    const parsed = parseAeroConfigQueryOverrides("?vmRuntime=machine");
    expect(parsed.lockedKeys.has("vmRuntime")).toBe(true);
    expect(parsed.overrides.vmRuntime).toBe("machine");
  });

  it("query parsing: accepts machine=1 shorthand", () => {
    const parsed = parseAeroConfigQueryOverrides("?machine=1");
    expect(parsed.lockedKeys.has("vmRuntime")).toBe(true);
    expect(parsed.overrides.vmRuntime).toBe("machine");
  });

  it("query parsing: invalid vm runtime produces issue and falls back to default", () => {
    const caps = {
      supportsThreadedWorkers: true,
      threadedWorkersUnsupportedReason: null,
      supportsWebGPU: false,
      webgpuUnsupportedReason: "no webgpu",
    };
    const resolved = resolveAeroConfigFromSources({ capabilities: caps, queryString: "?vm=invalid" });
    expect(resolved.effective.vmRuntime).toBe("legacy");
    expect(resolved.issues.some((i) => i.key === "vmRuntime")).toBe(true);
    expect(resolved.lockedKeys.has("vmRuntime")).toBe(false);
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
