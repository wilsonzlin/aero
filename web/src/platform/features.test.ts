import { afterEach, describe, expect, it } from "vitest";

import { detectPlatformFeatures, explainMissingRequirements, type PlatformFeatureReport } from "./features";

const GLOBALS = globalThis as unknown as {
  crossOriginIsolated?: boolean;
  isSecureContext?: boolean;
  AudioWorkletNode?: unknown;
  AudioContext?: unknown;
  OffscreenCanvas?: unknown;
  FileSystemFileHandle?: unknown;
};

const originalValidate = WebAssembly.validate;

afterEach(() => {
  WebAssembly.validate = originalValidate;
  delete GLOBALS.crossOriginIsolated;
  delete GLOBALS.isSecureContext;

  // Node.js provides a `navigator` getter; it is extensible, so we can delete any
  // stubbed properties after each test.
  if (typeof navigator !== "undefined") {
    delete (navigator as unknown as { gpu?: unknown }).gpu;
    delete (navigator as unknown as { storage?: unknown }).storage;
    delete (navigator as unknown as { usb?: unknown }).usb;
    delete (navigator as unknown as { hid?: unknown }).hid;
  }

  delete GLOBALS.AudioWorkletNode;
  delete GLOBALS.AudioContext;
  delete GLOBALS.OffscreenCanvas;
  delete GLOBALS.FileSystemFileHandle;
});

function report(overrides: Partial<PlatformFeatureReport> = {}): PlatformFeatureReport {
  return {
    crossOriginIsolated: false,
    sharedArrayBuffer: false,
    wasmSimd: false,
    wasmThreads: false,
    jit_dynamic_wasm: false,
    webgpu: false,
    webusb: false,
    webhid: false,
    webgl2: false,
    opfs: false,
    opfsSyncAccessHandle: false,
    audioWorklet: false,
    offscreenCanvas: false,
    ...overrides,
  };
}

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

    GLOBALS.crossOriginIsolated = true;

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
    (navigator as unknown as { hid?: unknown }).hid = {};
    (navigator as unknown as { storage?: unknown }).storage = {
      getDirectory: () => Promise.resolve(null),
    };

    GLOBALS.AudioWorkletNode = class AudioWorkletNode {};
    GLOBALS.AudioContext = class AudioContext {};
    GLOBALS.OffscreenCanvas = class OffscreenCanvas {};

    const report = detectPlatformFeatures();
    expect(report.webgpu).toBe(true);
    expect(report.webusb).toBe(true);
    expect(report.webhid).toBe(true);
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
    GLOBALS.isSecureContext = true;
    const secure = detectPlatformFeatures();
    expect(secure.webusb).toBe(true);
  });

  it("detects WebHID only in secure contexts with navigator.hid exposed", () => {
    WebAssembly.validate = (() => false) as typeof WebAssembly.validate;

    (navigator as unknown as { hid?: unknown }).hid = {};
    const insecure = detectPlatformFeatures();
    expect(insecure.webhid).toBe(false);

    GLOBALS.isSecureContext = true;
    const secure = detectPlatformFeatures();
    expect(secure.webhid).toBe(true);
  });

  it("detects OPFS SyncAccessHandle when FileSystemFileHandle.createSyncAccessHandle is exposed", () => {
    WebAssembly.validate = (() => false) as typeof WebAssembly.validate;

    (navigator as unknown as { storage?: unknown }).storage = {
      getDirectory: () => Promise.resolve(null),
    };

    // `createSyncAccessHandle` is only usable in dedicated workers at runtime, but for feature
    // detection we only need to check that the method exists on the prototype.
    GLOBALS.FileSystemFileHandle = class FileSystemFileHandle {
      createSyncAccessHandle(): void {}
    };

    const report = detectPlatformFeatures();
    expect(report.opfs).toBe(true);
    expect(report.opfsSyncAccessHandle).toBe(true);
  });
});

describe("explainMissingRequirements", () => {
  it("returns no messages when all requirements are satisfied", () => {
    expect(
      explainMissingRequirements(
        report({
          crossOriginIsolated: true,
          sharedArrayBuffer: true,
          wasmSimd: true,
          wasmThreads: true,
          jit_dynamic_wasm: true,
          webgpu: true,
          webgl2: true,
          opfs: true,
          opfsSyncAccessHandle: true,
          audioWorklet: true,
          offscreenCanvas: true,
        }),
      ),
    ).toEqual([]);
  });

  it("includes OPFS SyncAccessHandle when OPFS is available but sync handles are not", () => {
    const messages = explainMissingRequirements(
      report({
        crossOriginIsolated: true,
        sharedArrayBuffer: true,
        wasmSimd: true,
        wasmThreads: true,
        jit_dynamic_wasm: true,
        webgpu: true,
        webgl2: true,
        opfs: true,
        opfsSyncAccessHandle: false,
        audioWorklet: true,
        offscreenCanvas: true,
      }),
    );

    expect(messages.join("\n")).toContain("SyncAccessHandle");
    expect(messages.join("\n")).toContain("IndexedDB");
  });

  it("returns actionable messages for missing capabilities", () => {
    const messages = explainMissingRequirements(report());

    // Keep this intentionally broad (copy edits shouldn't break tests).
    expect(messages).toHaveLength(10);
    expect(messages.join("\n")).toContain("cross-origin isolated");
    expect(messages.join("\n")).toContain("SharedArrayBuffer");
    expect(messages.join("\n")).toContain("WebAssembly SIMD");
    expect(messages.join("\n")).toContain("Dynamic WebAssembly compilation");
    expect(messages.join("\n")).toContain("WebGPU");
    expect(messages.join("\n")).toContain("WebGL2");
    expect(messages.join("\n")).toContain("wasm-unsafe-eval");
    expect(messages.join("\n")).toContain("SyncAccessHandle");
  });
});
