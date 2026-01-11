import { afterEach, describe, expect, it, vi } from "vitest";

import { initWasm } from "./wasm_loader";
import { precompileWasm } from "./wasm_preload";

// Empty (but valid) WASM module: just the header.
const WASM_EMPTY_MODULE_BYTES = new Uint8Array([0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]);

const originalFetch = globalThis.fetch;
const originalBinaryOverride = (globalThis as any).__aeroWasmBinaryUrlOverride;
const originalJsOverride = (globalThis as any).__aeroWasmJsImporterOverride;

afterEach(() => {
  if (originalFetch) {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    (globalThis as any).fetch = originalFetch;
  } else {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    delete (globalThis as any).fetch;
  }

  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  (globalThis as any).__aeroWasmBinaryUrlOverride = originalBinaryOverride;
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  (globalThis as any).__aeroWasmJsImporterOverride = originalJsOverride;

  vi.restoreAllMocks();
});

describe("runtime/wasm_preload", () => {
  it("precompiles a module and allows initWasm({ module }) to instantiate", async () => {
    const url = "https://example.invalid/aero_wasm_bg.wasm";

    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    (globalThis as any).__aeroWasmBinaryUrlOverride = { single: url };

    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    (globalThis as any).fetch = vi.fn(async (requested: unknown) => {
      expect(String(requested)).toBe(url);
      return new Response(WASM_EMPTY_MODULE_BYTES, {
        status: 200,
        headers: { "Content-Type": "application/wasm" },
      });
    });

    let initInput: unknown;
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    (globalThis as any).__aeroWasmJsImporterOverride = {
      single: async () => ({
        default: async (input?: unknown) => {
          initInput = input;
          if (input instanceof WebAssembly.Module) {
            await WebAssembly.instantiate(input, {});
          }
        },
        greet: (name: string) => `hello ${name}`,
        add: (a: number, b: number) => a + b,
        version: () => 1,
        sum: (a: number, b: number) => a + b,
      }),
    };

    const compiled = await precompileWasm("single");
    expect(compiled.url).toBe(url);
    expect(compiled.module).toBeInstanceOf(WebAssembly.Module);

    const { api, variant } = await initWasm({ module: compiled.module });
    expect(variant).toBe("single");
    expect(api.add(2, 3)).toBe(5);
    expect(initInput).toBe(compiled.module);
  });
});

