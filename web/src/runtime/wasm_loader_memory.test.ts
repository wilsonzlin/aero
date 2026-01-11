import { describe, expect, it } from "vitest";

import { initWasm } from "./wasm_loader";
import { computeGuestRamLayout, guestToLinear } from "./shared_layout";

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

describe("runtime/wasm_loader (memory injection)", () => {
  it("wires the provided WebAssembly.Memory as linear memory", async () => {
    if (!sharedMemorySupported()) return;

    // `initWasm` only selects the threaded build when `crossOriginIsolated` is
    // true. In Node/Vitest that flag is absent, so we spoof it for this test.
    const hadCrossOriginIsolated = Object.prototype.hasOwnProperty.call(globalThis, "crossOriginIsolated");
    const originalCrossOriginIsolated = (globalThis as any).crossOriginIsolated;
    (globalThis as any).crossOriginIsolated = true;

    try {
      // Avoid poking at the low addresses reserved for the Rust/WASM runtime by
      // probing inside the guest RAM region.
      const desiredGuestBytes = 1 * 1024 * 1024;
      const layout = computeGuestRamLayout(desiredGuestBytes);
      // Keep the memory fixed-size: growing a shared WebAssembly.Memory can
      // replace the underlying SharedArrayBuffer, invalidating existing views.
      const memory = new WebAssembly.Memory({ initial: layout.wasm_pages, maximum: layout.wasm_pages, shared: true });

      try {
        const { api } = await initWasm({ variant: "threaded", memory });

        const view = new DataView(memory.buffer);
        const offset = guestToLinear(layout, 0x100);

        view.setUint32(offset, 0x11223344, true);
        expect(api.mem_load_u32(offset)).toBe(0x11223344);

        api.mem_store_u32(offset, 0x55667788);
        expect(view.getUint32(offset, true)).toBe(0x55667788);
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        // The wasm-pack output is generated and may be absent in some test
        // environments; skip rather than failing unrelated suites.
        if (message.includes("Missing threaded WASM package")) return;
        throw err;
      }
    } finally {
      if (hadCrossOriginIsolated) {
        (globalThis as any).crossOriginIsolated = originalCrossOriginIsolated;
      } else {
        delete (globalThis as any).crossOriginIsolated;
      }
    }
  });
});
