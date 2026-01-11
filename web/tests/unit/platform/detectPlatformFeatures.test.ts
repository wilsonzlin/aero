import { afterEach, describe, expect, it } from "vitest";

import { detectPlatformFeatures } from "../../../src/platform/features";

const originalValidate = WebAssembly.validate;

afterEach(() => {
  WebAssembly.validate = originalValidate;
  delete (globalThis as typeof globalThis & { crossOriginIsolated?: boolean }).crossOriginIsolated;
  delete (globalThis as typeof globalThis & { isSecureContext?: boolean }).isSecureContext;

  // Node.js provides a `navigator` getter; it is extensible, so we can delete any
  // stubbed properties after each test.
  if (typeof navigator !== "undefined") {
    delete (navigator as unknown as { gpu?: unknown }).gpu;
    delete (navigator as unknown as { storage?: unknown }).storage;
    delete (navigator as unknown as { usb?: unknown }).usb;
  }

  delete (globalThis as typeof globalThis & { AudioWorkletNode?: unknown }).AudioWorkletNode;
  delete (globalThis as typeof globalThis & { AudioContext?: unknown }).AudioContext;
  delete (globalThis as typeof globalThis & { OffscreenCanvas?: unknown }).OffscreenCanvas;
});

describe("detectPlatformFeatures", () => {
  it("treats WASM threads as requiring crossOriginIsolated, SharedArrayBuffer, and Atomics", () => {
    let validateCalls = 0;
    WebAssembly.validate = (() => {
      validateCalls += 1;
      return true;
    }) as typeof WebAssembly.validate;

    // Node provides SharedArrayBuffer + Atomics but is not cross-origin isolated by default.
    const baseline = detectPlatformFeatures();
    expect(baseline.crossOriginIsolated).toBe(false);
    expect(baseline.sharedArrayBuffer).toBe(true);
    expect(baseline.wasmSimd).toBe(true);
    expect(baseline.wasmThreads).toBe(false);

    (globalThis as typeof globalThis & { crossOriginIsolated?: boolean }).crossOriginIsolated = true;

    const isolated = detectPlatformFeatures();
    expect(isolated.crossOriginIsolated).toBe(true);
    expect(isolated.sharedArrayBuffer).toBe(true);
    expect(isolated.wasmThreads).toBe(true);

    expect(validateCalls).toBeGreaterThan(0);
  });

  it("detects exposed browser APIs via globals (navigator.*, Audio*, OffscreenCanvas)", () => {
    WebAssembly.validate = (() => false) as typeof WebAssembly.validate;
    (globalThis as typeof globalThis & { crossOriginIsolated?: boolean }).crossOriginIsolated = true;
    (globalThis as typeof globalThis & { isSecureContext?: boolean }).isSecureContext = true;

    // Node's global `navigator` is extensible. Stub the fields used by our detector.
    (navigator as unknown as { gpu?: unknown }).gpu = {};
    (navigator as unknown as { usb?: unknown }).usb = {};
    (navigator as unknown as { storage?: unknown }).storage = {
      getDirectory: () => Promise.resolve(null),
    };

    (globalThis as typeof globalThis & { AudioWorkletNode?: unknown }).AudioWorkletNode = class AudioWorkletNode {};
    (globalThis as typeof globalThis & { AudioContext?: unknown }).AudioContext = class AudioContext {};
    (globalThis as typeof globalThis & { OffscreenCanvas?: unknown }).OffscreenCanvas = class OffscreenCanvas {};

    const report = detectPlatformFeatures();
    expect(report.webgpu).toBe(true);
    expect(report.webusb).toBe(true);
    expect(report.opfs).toBe(true);
    expect(report.audioWorklet).toBe(true);
    expect(report.offscreenCanvas).toBe(true);

    // We explicitly forced these via stubs above.
    expect(report.crossOriginIsolated).toBe(true);
    expect(report.wasmSimd).toBe(false);
  });

  it("detects WebUSB only in secure contexts with navigator.usb exposed", () => {
    WebAssembly.validate = (() => false) as typeof WebAssembly.validate;

    // Baseline: insecure context (default in Node) => WebUSB gated off.
    (navigator as unknown as { usb?: unknown }).usb = {};
    const insecure = detectPlatformFeatures();
    expect(insecure.webusb).toBe(false);

    // Secure context + navigator.usb => WebUSB available.
    (globalThis as typeof globalThis & { isSecureContext?: boolean }).isSecureContext = true;
    const secure = detectPlatformFeatures();
    expect(secure.webusb).toBe(true);
  });
});
