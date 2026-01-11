import { describe, expect, it } from "vitest";

import { isJitCompileRequest, isJitWorkerResponse } from "./jit_protocol";

describe("jit_protocol", () => {
  it("validates jit:compile requests", () => {
    const ok = { type: "jit:compile", id: 1, wasmBytes: new ArrayBuffer(8) };
    expect(isJitCompileRequest(ok)).toBe(true);

    expect(isJitCompileRequest({ type: "jit:compile", id: 1, wasmBytes: new Uint8Array(8) })).toBe(false);
    expect(isJitCompileRequest({ type: "jit:compile", id: "1", wasmBytes: new ArrayBuffer(8) })).toBe(false);
    expect(isJitCompileRequest({ type: "jit:compiled", id: 1, wasmBytes: new ArrayBuffer(8) })).toBe(false);
  });

  it("validates jit:compiled/jit:error responses", () => {
    const bytes = new Uint8Array([0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]);
    const module = new WebAssembly.Module(bytes);
    expect(isJitWorkerResponse({ type: "jit:compiled", id: 7, module, durationMs: 1.25, cached: true })).toBe(true);

    expect(isJitWorkerResponse({ type: "jit:error", id: 7, message: "nope", code: "csp_blocked", durationMs: 0 })).toBe(
      true,
    );

    expect(isJitWorkerResponse({ type: "jit:compiled", id: 7, durationMs: 1.25 })).toBe(false);
    expect(isJitWorkerResponse({ type: "jit:error", id: 7, message: 123 })).toBe(false);
  });
});
