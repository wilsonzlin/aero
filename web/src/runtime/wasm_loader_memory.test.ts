import { describe, expect, it } from "vitest";

import { initWasm } from "./wasm_loader";
import { assertWasmMemoryWiring } from "./wasm_memory_probe";
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

    // In browsers, `crossOriginIsolated` must be true for SharedArrayBuffer/WASM
    // threads. Spoof it here so the test exercises the same (web-like) path
    // under Node/Vitest.
    const hadCrossOriginIsolated = Object.prototype.hasOwnProperty.call(globalThis, "crossOriginIsolated");
    const originalCrossOriginIsolated = (globalThis as any).crossOriginIsolated;
    (globalThis as any).crossOriginIsolated = true;

    try {
      // Avoid poking at the low addresses reserved for the Rust/WASM runtime by
      // probing inside the guest RAM region.
      const desiredGuestBytes = 1 * 1024 * 1024;
      const layout = computeGuestRamLayout(desiredGuestBytes);
      // Keep the memory fixed-size (`maximum === initial`) so we don't reserve a
      // 4GiB virtual address space unnecessarily, and because growing a shared
      // WebAssembly.Memory can replace the underlying SharedArrayBuffer,
      // invalidating existing views.
      const memory = new WebAssembly.Memory({ initial: layout.wasm_pages, maximum: layout.wasm_pages, shared: true });

      try {
        const { api } = await initWasm({ variant: "threaded", memory });
        const offset = guestToLinear(layout, 0x100);
        assertWasmMemoryWiring({ api, memory, linearOffset: offset, context: "wasm_loader_memory.test" });
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
