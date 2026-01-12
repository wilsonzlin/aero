import { afterEach, describe, expect, it } from "vitest";

import { initJitWasm } from "./jit_wasm_loader";

// Empty (but valid) WASM module: just the header.
const WASM_EMPTY_MODULE_BYTES = new Uint8Array([0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]);

const originalJsOverride = (globalThis as any).__aeroJitWasmJsImporterOverride;

afterEach(() => {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  (globalThis as any).__aeroJitWasmJsImporterOverride = originalJsOverride;
});

describe("runtime/jit_wasm_loader", () => {
  it("exposes compile_tier1_block and returns valid wasm bytes", async () => {
    const module = await WebAssembly.compile(WASM_EMPTY_MODULE_BYTES);

    const fakeModule = {
      default: async (_input?: unknown) => {},
      compile_tier1_block: () => new Uint8Array(WASM_EMPTY_MODULE_BYTES),
    };

    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    (globalThis as any).__aeroJitWasmJsImporterOverride = {
      single: async () => fakeModule,
      threaded: async () => fakeModule,
    };

    const { api } = await initJitWasm({ module });
    const bytes = api.compile_tier1_block();
    // `WebAssembly.validate` expects an ArrayBuffer-backed view; `Uint8Array` is
    // generic over `ArrayBufferLike` and may be backed by `SharedArrayBuffer`.
    // Copy when needed so TypeScript (and spec compliance) are happy.
    const bytesForWasm: Uint8Array<ArrayBuffer> =
      bytes.buffer instanceof ArrayBuffer ? (bytes as Uint8Array<ArrayBuffer>) : (new Uint8Array(bytes) as Uint8Array<ArrayBuffer>);
    expect(WebAssembly.validate(bytesForWasm)).toBe(true);
  });
});
