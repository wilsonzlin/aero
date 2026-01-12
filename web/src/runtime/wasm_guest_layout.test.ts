import { describe, expect, it } from "vitest";

import { computeGuestRamLayout, guestToLinear } from "./shared_layout";
import { assertWasmMemoryWiring } from "./wasm_memory_probe";
import { initWasm } from "./wasm_loader";

function sharedMemorySupported(): boolean {
  if (typeof WebAssembly === "undefined" || typeof WebAssembly.Memory !== "function") return false;
  if (typeof SharedArrayBuffer === "undefined") return false;
  try {
    // eslint-disable-next-line no-new
    const mem = new WebAssembly.Memory({ initial: 1, maximum: 1, shared: true });
    return mem.buffer instanceof SharedArrayBuffer;
  } catch {
    return false;
  }
}

describe("runtime/wasm_guest_layout", () => {
  it("maps guest physical memory into wasm linear memory after the runtime reserved region", async () => {
    const desiredGuestBytes = 1 * 1024 * 1024;
    const jsLayout = computeGuestRamLayout(desiredGuestBytes);

    const variants: Array<"threaded" | "single"> = sharedMemorySupported() ? ["threaded", "single"] : ["single"];

    for (const variant of variants) {
      // In browsers, `crossOriginIsolated` must be true for SharedArrayBuffer/WASM
      // threads. Spoof it here so the test exercises the same (web-like) path
      // under Node/Vitest.
      const hadCrossOriginIsolated = Object.prototype.hasOwnProperty.call(globalThis, "crossOriginIsolated");
      const originalCrossOriginIsolated = (globalThis as any).crossOriginIsolated;
      if (variant === "threaded") {
        (globalThis as any).crossOriginIsolated = true;
      }

      try {
        const memory = new WebAssembly.Memory({
          initial: jsLayout.wasm_pages,
          maximum: jsLayout.wasm_pages,
          ...(variant === "threaded" ? { shared: true } : {}),
        });

        const { api } = await initWasm({ variant, memory });

        const wasmLayout = api.guest_ram_layout(desiredGuestBytes);
        expect(wasmLayout.guest_base >>> 0).toBe(jsLayout.guest_base);
        expect(wasmLayout.guest_size >>> 0).toBe(jsLayout.guest_size);
        expect(wasmLayout.runtime_reserved >>> 0).toBe(jsLayout.runtime_reserved);

        const paddr = 0x100;
        const linear = guestToLinear(jsLayout, paddr);
        const dv = new DataView(memory.buffer);
        dv.setUint32(linear, 0x12345678, true);

        expect(api.mem_load_u32(linear) >>> 0).toBe(0x12345678);
        assertWasmMemoryWiring({ api, memory, linearOffset: linear, context: `wasm_guest_layout.test:${variant}` });

        expect(() => guestToLinear(jsLayout, -1)).toThrow();
        expect(() => guestToLinear(jsLayout, jsLayout.guest_size)).toThrow();
        return;
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        // The wasm-pack output is generated and may be absent in some test
        // environments; skip rather than failing unrelated suites.
        const missingMessage =
          variant === "threaded" ? "Missing threaded WASM package" : "Missing single-thread WASM package";
        if (message.includes(missingMessage)) {
          continue;
        }
        throw err;
      } finally {
        if (variant === "threaded") {
          if (hadCrossOriginIsolated) {
            (globalThis as any).crossOriginIsolated = originalCrossOriginIsolated;
          } else {
            delete (globalThis as any).crossOriginIsolated;
          }
        }
      }
    }
  });
});
