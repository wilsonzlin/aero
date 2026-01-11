import { describe, expect, it } from "vitest";

import { computeGuestRamLayout, guestToLinear } from "./shared_layout";

const wasmImporters = import.meta.glob("../wasm/pkg-*/aero_wasm.js");

type WasmImporter = (() => Promise<any>) | undefined;

function pickImporter(paths: string[]): WasmImporter {
  for (const p of paths) {
    const importer = wasmImporters[p];
    if (importer) return importer;
  }
  return undefined;
}

const THREADED_IMPORTER = pickImporter(["../wasm/pkg-threaded/aero_wasm.js", "../wasm/pkg-threaded-dev/aero_wasm.js"]);
const SINGLE_IMPORTER = pickImporter(["../wasm/pkg-single/aero_wasm.js", "../wasm/pkg-single-dev/aero_wasm.js"]);

async function initWithImportedMemory(mod: any, memory: WebAssembly.Memory): Promise<void> {
  // wasm-bindgen's init signature varies slightly across versions; support the
  // common `(input?, memory?)` form used for `--import-memory` builds.
  try {
    await mod.default(undefined, memory);
    return;
  } catch {
    // Fallback for older glue code that takes an options object.
    await mod.default({ memory });
  }
}

describe.runIf(Boolean(THREADED_IMPORTER || SINGLE_IMPORTER))("runtime/wasm_guest_layout", () => {
  it("maps guest physical memory into wasm linear memory after the runtime reserved region", async () => {
    const variant = THREADED_IMPORTER ? "threaded" : "single";
    const importer = (THREADED_IMPORTER ?? SINGLE_IMPORTER)!;
    const mod = await importer();

    const desiredGuestBytes = 1 * 1024 * 1024;
    const jsLayout = computeGuestRamLayout(desiredGuestBytes);

    const memory = new WebAssembly.Memory({
      initial: jsLayout.wasm_pages,
      maximum: jsLayout.wasm_pages,
      ...(variant === "threaded" ? { shared: true } : {}),
    });

    await initWithImportedMemory(mod, memory);

    expect(typeof mod.guest_ram_layout).toBe("function");
    expect(typeof mod.mem_load_u32).toBe("function");

    const wasmLayout = mod.guest_ram_layout(desiredGuestBytes);
    expect(wasmLayout.guest_base >>> 0).toBe(jsLayout.guest_base);
    expect(wasmLayout.guest_size >>> 0).toBe(jsLayout.guest_size);
    expect(wasmLayout.runtime_reserved >>> 0).toBe(jsLayout.runtime_reserved);

    const paddr = 0x100;
    const linear = guestToLinear(jsLayout, paddr);
    const dv = new DataView(memory.buffer);
    dv.setUint32(linear, 0x12345678, true);

    expect(mod.mem_load_u32(linear) >>> 0).toBe(0x12345678);

    expect(() => guestToLinear(jsLayout, -1)).toThrow();
    expect(() => guestToLinear(jsLayout, jsLayout.guest_size)).toThrow();
  });
});
