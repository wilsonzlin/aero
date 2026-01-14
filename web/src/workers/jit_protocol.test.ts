import { describe, expect, it } from "vitest";

import { isJitCompileRequest, isJitTier1CompileRequest, isJitWorkerResponse } from "./jit_protocol";

describe("jit_protocol", () => {
  it("validates jit:compile requests", () => {
    const ok = { type: "jit:compile", id: 1, wasmBytes: new ArrayBuffer(8) };
    expect(isJitCompileRequest(ok)).toBe(true);

    expect(isJitCompileRequest({ type: "jit:compile", id: 1, wasmBytes: new Uint8Array(8) })).toBe(false);
    expect(isJitCompileRequest({ type: "jit:compile", id: "1", wasmBytes: new ArrayBuffer(8) })).toBe(false);
    expect(isJitCompileRequest({ type: "jit:compiled", id: 1, wasmBytes: new ArrayBuffer(8) })).toBe(false);
  });

  it("validates jit:tier1 requests", () => {
    const bytes = new Uint8Array([0xc3]);
    expect(
      isJitTier1CompileRequest({
        type: "jit:tier1",
        id: 1,
        entryRip: 0n,
        codeBytes: bytes,
        maxBytes: 1024,
        bitness: 64,
        memoryShared: true,
      }),
    ).toBe(true);

    // Code bytes optional (worker can snapshot from shared guest memory).
    expect(
      isJitTier1CompileRequest({ type: "jit:tier1", id: 1, entryRip: 0, maxBytes: 1024, bitness: 32, memoryShared: true }),
    ).toBe(true);

    expect(
      isJitTier1CompileRequest({
        type: "jit:tier1",
        id: 1,
        entryRip: 0,
        codeBytes: new ArrayBuffer(8),
        maxBytes: 1024,
        bitness: 64,
        memoryShared: true,
      }),
    ).toBe(false);
  });

  it("validates jit:compiled/jit:tier1:compiled/jit:error responses", () => {
    const bytes = new Uint8Array([0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]);
    const module = new WebAssembly.Module(bytes);
    expect(isJitWorkerResponse({ type: "jit:compiled", id: 7, module, durationMs: 1.25, cached: true })).toBe(true);
    expect(
      isJitWorkerResponse({
        type: "jit:tier1:compiled",
        id: 8,
        entryRip: 0,
        module,
        codeByteLen: 1,
        exitToInterpreter: false,
      }),
    ).toBe(true);
    expect(
      isJitWorkerResponse({
        type: "jit:tier1:compiled",
        id: 9,
        entryRip: 0n,
        wasmBytes: bytes.buffer,
        codeByteLen: 1,
        exitToInterpreter: false,
      }),
    ).toBe(true);

    expect(isJitWorkerResponse({ type: "jit:error", id: 7, message: "nope", code: "csp_blocked", durationMs: 0 })).toBe(
      true,
    );

    expect(isJitWorkerResponse({ type: "jit:compiled", id: 7, durationMs: 1.25 })).toBe(false);
    expect(isJitWorkerResponse({ type: "jit:error", id: 7, message: 123 })).toBe(false);
  });
});
