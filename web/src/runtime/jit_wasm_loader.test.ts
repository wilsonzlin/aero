import { afterEach, describe, expect, it, vi } from "vitest";

// Empty (but valid) WASM module: just the header.
const WASM_EMPTY_MODULE_BYTES = new Uint8Array([0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]);

const originalJsOverride = (globalThis as any).__aeroJitWasmJsImporterOverride;
const originalCrossOriginIsolatedDescriptor = Object.getOwnPropertyDescriptor(globalThis, "crossOriginIsolated");
const originalWasmMemory = WebAssembly.Memory;

function restoreCrossOriginIsolated(): void {
  if (originalCrossOriginIsolatedDescriptor) {
    Object.defineProperty(globalThis, "crossOriginIsolated", originalCrossOriginIsolatedDescriptor);
  } else {
    Reflect.deleteProperty(globalThis as unknown as { crossOriginIsolated?: unknown }, "crossOriginIsolated");
  }
}

function restoreWasmMemory(): void {
  Object.defineProperty(WebAssembly, "Memory", {
    value: originalWasmMemory,
    writable: true,
    configurable: true,
  });
}

afterEach(() => {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  (globalThis as any).__aeroJitWasmJsImporterOverride = originalJsOverride;
  restoreCrossOriginIsolated();
  restoreWasmMemory();
  vi.resetModules();
  vi.clearAllMocks();
});

describe("runtime/jit_wasm_loader", () => {
  it("exposes compile_tier1_block and returns valid wasm bytes", async () => {
    const module = await WebAssembly.compile(WASM_EMPTY_MODULE_BYTES);

    const fakeModule = {
      default: async (_input?: unknown) => {},
      compile_tier1_block: () => ({
        wasm_bytes: new Uint8Array(WASM_EMPTY_MODULE_BYTES),
        code_byte_len: 0,
        exit_to_interpreter: false,
      }),
    };

    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    (globalThis as any).__aeroJitWasmJsImporterOverride = {
      single: async () => fakeModule,
      threaded: async () => fakeModule,
    };

    const { initJitWasm } = await import("./jit_wasm_loader");
    const { api } = await initJitWasm({ module });
    const { wasm_bytes: bytes } = api.compile_tier1_block();
    // `WebAssembly.validate` expects an ArrayBuffer-backed view; `Uint8Array` is
    // generic over `ArrayBufferLike` and may be backed by `SharedArrayBuffer`.
    // Copy when needed so TypeScript (and spec compliance) are happy.
    const bytesForWasm: Uint8Array<ArrayBuffer> =
      bytes.buffer instanceof ArrayBuffer ? (bytes as Uint8Array<ArrayBuffer>) : (new Uint8Array(bytes) as Uint8Array<ArrayBuffer>);
    expect(WebAssembly.validate(bytesForWasm)).toBe(true);
  });

  it("prefers pkg-jit-single even when WASM threads are available (avoids huge SharedArrayBuffer allocation)", async () => {
    // Simulate a COOP/COEP-capable environment (threads available).
    Object.defineProperty(globalThis, "crossOriginIsolated", {
      value: true,
      configurable: true,
      enumerable: true,
      writable: true,
    });

    const allocations: Array<{ initial?: number; maximum?: number; shared?: boolean }> = [];

    // Guard against accidentally allocating multi-GiB shared memories (the bug).
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const PatchedMemory = function (this: any, descriptor: any) {
      allocations.push(descriptor ?? {});
      if (descriptor?.shared && typeof descriptor.maximum === "number" && descriptor.maximum > 1024) {
        throw new Error(`attempted to allocate large shared WebAssembly.Memory: maximum=${descriptor.maximum} pages`);
      }
      // eslint-disable-next-line @typescript-eslint/no-unsafe-argument
      return new originalWasmMemory(descriptor);
    } as unknown as typeof WebAssembly.Memory;
    // Preserve `instanceof WebAssembly.Memory` checks.
    (PatchedMemory as unknown as { prototype: unknown }).prototype = originalWasmMemory.prototype;
    Object.defineProperty(WebAssembly, "Memory", {
      value: PatchedMemory,
      writable: true,
      configurable: true,
    });

    let threadedImporterCalls = 0;
    const fakeSingleModule = {
      default: async (_input?: unknown) => {},
      compile_tier1_block: () => ({
        wasm_bytes: new Uint8Array(WASM_EMPTY_MODULE_BYTES),
        code_byte_len: 0,
        exit_to_interpreter: false,
      }),
    };

    // If this ever gets loaded, it will try to allocate a huge shared memory (and the patched
    // `WebAssembly.Memory` will throw).
    const fakeThreadedModule = {
      default: async (_input?: unknown) => {
        // 65536 pages = 4GiB.
        // eslint-disable-next-line no-new
        new WebAssembly.Memory({ initial: 1, maximum: 65536, shared: true });
      },
      compile_tier1_block: () => ({
        wasm_bytes: new Uint8Array(WASM_EMPTY_MODULE_BYTES),
        code_byte_len: 0,
        exit_to_interpreter: false,
      }),
    };

    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    (globalThis as any).__aeroJitWasmJsImporterOverride = {
      single: async () => fakeSingleModule,
      threaded: async () => {
        threadedImporterCalls += 1;
        return fakeThreadedModule;
      },
    };

    const { initJitWasm } = await import("./jit_wasm_loader");
    const module = await WebAssembly.compile(WASM_EMPTY_MODULE_BYTES);
    const { api } = await initJitWasm({ module });

    expect(threadedImporterCalls).toBe(0);
    expect(allocations.some((a) => !!a.shared && typeof a.maximum === "number" && a.maximum > 1024)).toBe(false);

    const { wasm_bytes: bytes } = api.compile_tier1_block();
    const bytesForWasm: Uint8Array<ArrayBuffer> =
      bytes.buffer instanceof ArrayBuffer ? (bytes as Uint8Array<ArrayBuffer>) : (new Uint8Array(bytes) as Uint8Array<ArrayBuffer>);
    expect(WebAssembly.validate(bytesForWasm)).toBe(true);
  });
});
