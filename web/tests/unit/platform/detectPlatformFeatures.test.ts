import { afterEach, describe, expect, it, vi } from "vitest";

import { detectPlatformFeatures } from "../../../src/platform/features";

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("detectPlatformFeatures", () => {
  it("treats WASM threads as requiring crossOriginIsolated, SharedArrayBuffer, and Atomics", () => {
    const validateSpy = vi.spyOn(WebAssembly, "validate").mockReturnValue(true);

    // Node provides SharedArrayBuffer + Atomics but is not cross-origin isolated by default.
    const baseline = detectPlatformFeatures();
    expect(baseline.crossOriginIsolated).toBe(false);
    expect(baseline.sharedArrayBuffer).toBe(true);
    expect(baseline.wasmSimd).toBe(true);
    expect(baseline.wasmThreads).toBe(false);

    vi.stubGlobal("crossOriginIsolated", true);

    const isolated = detectPlatformFeatures();
    expect(isolated.crossOriginIsolated).toBe(true);
    expect(isolated.sharedArrayBuffer).toBe(true);
    expect(isolated.wasmThreads).toBe(true);

    expect(validateSpy).toHaveBeenCalled();
  });

  it("detects exposed browser APIs via globals (navigator.*, Audio*, OffscreenCanvas)", () => {
    vi.spyOn(WebAssembly, "validate").mockReturnValue(false);
    vi.stubGlobal("crossOriginIsolated", true);

    vi.stubGlobal("navigator", {
      gpu: {},
      storage: {
        getDirectory: () => Promise.resolve(null),
      },
    });

    vi.stubGlobal("AudioWorkletNode", class AudioWorkletNode {});
    vi.stubGlobal("AudioContext", class AudioContext {});
    vi.stubGlobal("OffscreenCanvas", class OffscreenCanvas {});

    const report = detectPlatformFeatures();
    expect(report.webgpu).toBe(true);
    expect(report.opfs).toBe(true);
    expect(report.audioWorklet).toBe(true);
    expect(report.offscreenCanvas).toBe(true);

    // We explicitly forced these via stubs above.
    expect(report.crossOriginIsolated).toBe(true);
    expect(report.wasmSimd).toBe(false);
  });
});

