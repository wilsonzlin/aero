import { describe, expect, it } from "vitest";

import { explainMissingRequirements, type PlatformFeatureReport } from "../../../src/platform/features";

function report(overrides: Partial<PlatformFeatureReport> = {}): PlatformFeatureReport {
  return {
    crossOriginIsolated: false,
    sharedArrayBuffer: false,
    wasmSimd: false,
    wasmThreads: false,
    jit_dynamic_wasm: false,
    webgpu: false,
    webgl2: false,
    opfs: false,
    audioWorklet: false,
    offscreenCanvas: false,
    ...overrides,
  };
}

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
          audioWorklet: true,
          offscreenCanvas: true,
        }),
      ),
    ).toEqual([]);
  });

  it("returns actionable messages for missing capabilities", () => {
    const messages = explainMissingRequirements(report());

    // Keep this intentionally broad (copy edits shouldn't break tests).
    expect(messages).toHaveLength(9);
    expect(messages.join("\n")).toContain("cross-origin isolated");
    expect(messages.join("\n")).toContain("SharedArrayBuffer");
    expect(messages.join("\n")).toContain("WebAssembly SIMD");
    expect(messages.join("\n")).toContain("WebGPU");
    expect(messages.join("\n")).toContain("WebGL2");
    expect(messages.join("\n")).toContain("wasm-unsafe-eval");
  });
});
