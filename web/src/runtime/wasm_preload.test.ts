import { afterEach, describe, expect, it, vi } from "vitest";

import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

import { initWasm } from "./wasm_loader";
import { precompileWasm } from "./wasm_preload";

// Empty (but valid) WASM module: just the header.
const WASM_EMPTY_MODULE_BYTES = new Uint8Array([0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]);

const originalFetchDescriptor = Object.getOwnPropertyDescriptor(globalThis, "fetch");
const originalBinaryOverride = globalThis.__aeroWasmBinaryUrlOverride;
const originalJsOverride = globalThis.__aeroWasmJsImporterOverride;

afterEach(() => {
  if (originalFetchDescriptor) {
    Object.defineProperty(globalThis, "fetch", originalFetchDescriptor);
  } else {
    Reflect.deleteProperty(globalThis, "fetch");
  }

  globalThis.__aeroWasmBinaryUrlOverride = originalBinaryOverride;
  globalThis.__aeroWasmJsImporterOverride = originalJsOverride;

  vi.restoreAllMocks();
});

describe("runtime/wasm_preload", () => {
  it("precompiles a module and allows initWasm({ module }) to instantiate", async () => {
    const url = "https://example.invalid/aero_wasm_bg.wasm";

    globalThis.__aeroWasmBinaryUrlOverride = { single: url };

    const fetchMock = vi.fn(async (requested: RequestInfo | URL) => {
      expect(String(requested)).toBe(url);
      return new Response(WASM_EMPTY_MODULE_BYTES, {
        status: 200,
        headers: { "Content-Type": "application/wasm" },
      });
    });
    globalThis.fetch = fetchMock as unknown as typeof fetch;

    let initInput: unknown;
    globalThis.__aeroWasmJsImporterOverride = {
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
        UsbPassthroughBridge: class {
          free(): void {}
        },
        WebUsbUhciPassthroughHarness: class {
          free(): void {}
        },
      }),
    };

    const compiled = await precompileWasm("single");
    expect(compiled.url).toBe(url);
    expect(compiled.module).toBeInstanceOf(WebAssembly.Module);

    const { api, variant } = await initWasm({ module: compiled.module });
    expect(variant).toBe("single");
    expect(api.add(2, 3)).toBe(5);
    expect(initInput).toBe(compiled.module);
    expect(api.UsbPassthroughBridge).toBeDefined();
    expect(api.WebUsbUhciPassthroughHarness).toBeDefined();
  });

  it("precompiles from a file: URL in Node without using fetch()", async () => {
    const dir = await mkdtemp(join(tmpdir(), "aero-wasm-preload-"));
    try {
      const wasmPath = join(dir, "aero_wasm_bg.wasm");
      await writeFile(wasmPath, WASM_EMPTY_MODULE_BYTES);

      const url = pathToFileURL(wasmPath).toString();

      globalThis.__aeroWasmBinaryUrlOverride = { threaded: url };

      const fetchMock = vi.fn(async () => {
        throw new Error("fetch() should not be called for file: URLs in Node");
      });
      globalThis.fetch = fetchMock as unknown as typeof fetch;

      const compiled = await precompileWasm("threaded");
      expect(compiled.url).toBe(url);
      expect(compiled.module).toBeInstanceOf(WebAssembly.Module);

      expect(fetchMock).not.toHaveBeenCalled();
    } finally {
      await rm(dir, { recursive: true, force: true });
    }
  });

  it("precompiles from a Vite /@fs/ URL in Node without using fetch()", async () => {
    const dir = await mkdtemp(join(tmpdir(), "aero-wasm-preload-"));
    try {
      const wasmPath = join(dir, "aero_wasm_bg.wasm");
      await writeFile(wasmPath, WASM_EMPTY_MODULE_BYTES);

      // Vite can represent filesystem assets as `/@fs/<absolute-path>?url`.
      const url = `/@fs/${wasmPath.slice(1)}?url`;

      globalThis.__aeroWasmBinaryUrlOverride = { threaded: url };

      const fetchMock = vi.fn(async () => {
        throw new Error("fetch() should not be called for /@fs/ URLs in Node");
      });
      globalThis.fetch = fetchMock as unknown as typeof fetch;

      // `precompileWasm()` caches per variant, so load a fresh module instance.
      vi.resetModules();
      const { precompileWasm: precompileWasmFresh } = await import("./wasm_preload");

      const compiled = await precompileWasmFresh("threaded");
      expect(compiled.url).toBe(url);
      expect(compiled.module).toBeInstanceOf(WebAssembly.Module);

      expect(fetchMock).not.toHaveBeenCalled();
    } finally {
      await rm(dir, { recursive: true, force: true });
    }
  });
});
