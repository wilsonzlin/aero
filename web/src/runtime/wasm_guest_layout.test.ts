import { describe, expect, it } from "vitest";

import {
  GUEST_PCI_MMIO_BASE,
  HIGH_RAM_START,
  LOW_RAM_END,
  computeGuestRamLayout,
  guestPaddrToRamOffset,
  guestRangeInBounds,
  guestToLinear,
} from "./shared_layout";
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
  it("clamps guest RAM below the PCI MMIO BAR window (0xE0000000..0xFFFF_FFFF)", () => {
    const layout = computeGuestRamLayout(0xffff_ffff);
    expect(layout.guest_size).toBe(GUEST_PCI_MMIO_BASE);
    expect(layout.guest_base + layout.guest_size).toBeLessThanOrEqual(0x1_0000_0000);
  });

  it("guestPaddrToRamOffset maps small RAM configurations identity", () => {
    const layout = { guest_base: 0x1000, guest_size: 0x2000, runtime_reserved: 0, wasm_pages: 0 };
    expect(guestPaddrToRamOffset(layout, 0)).toBe(0);
    expect(guestPaddrToRamOffset(layout, 0x1234)).toBe(0x1234);
    expect(guestPaddrToRamOffset(layout, layout.guest_size - 1)).toBe(layout.guest_size - 1);
    expect(guestPaddrToRamOffset(layout, layout.guest_size)).toBeNull();
    expect(guestToLinear(layout, 0x10)).toBe(layout.guest_base + 0x10);
  });

  it("guestPaddrToRamOffset rejects the ECAM/PCI hole and maps high RAM above 4GiB", () => {
    const layout = {
      guest_base: 0x1000,
      guest_size: LOW_RAM_END + 0x2000,
      runtime_reserved: 0,
      wasm_pages: 0,
    };

    expect(guestPaddrToRamOffset(layout, 0)).toBe(0);
    expect(guestPaddrToRamOffset(layout, LOW_RAM_END)).toBeNull();
    expect(guestPaddrToRamOffset(layout, HIGH_RAM_START)).toBe(LOW_RAM_END);

    expect(() => guestToLinear(layout, LOW_RAM_END)).toThrow();
    expect(guestToLinear(layout, HIGH_RAM_START)).toBe(layout.guest_base + LOW_RAM_END);
  });

  it("guestRangeInBounds rejects ranges that touch the ECAM/PCI hole and accepts ranges in both RAM segments", () => {
    const layout = {
      guest_base: 0x1000,
      guest_size: LOW_RAM_END + 0x2000,
      runtime_reserved: 0,
      wasm_pages: 0,
    };

    // Low RAM.
    expect(guestRangeInBounds(layout, 0, 1)).toBe(true);
    expect(guestRangeInBounds(layout, LOW_RAM_END - 4, 4)).toBe(true);
    expect(guestRangeInBounds(layout, LOW_RAM_END - 4, 8)).toBe(false);

    // Hole.
    expect(guestRangeInBounds(layout, LOW_RAM_END, 1)).toBe(false);
    expect(guestRangeInBounds(layout, HIGH_RAM_START - 4, 4)).toBe(false);

    // High RAM.
    expect(guestRangeInBounds(layout, HIGH_RAM_START, 1)).toBe(true);
    expect(guestRangeInBounds(layout, HIGH_RAM_START + 0x1ff0, 0x10)).toBe(true);
    expect(guestRangeInBounds(layout, HIGH_RAM_START + 0x1ff0, 0x20)).toBe(false);

    // Zero-length ranges may sit on segment boundaries.
    expect(guestRangeInBounds(layout, LOW_RAM_END, 0)).toBe(true);
    expect(guestRangeInBounds(layout, HIGH_RAM_START, 0)).toBe(true);
  });

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

        // Large layout should be clamped the same way on the JS and WASM sides.
        // Do not allocate a huge WebAssembly.Memory; `guest_ram_layout` is a pure
        // computation and should not depend on the imported memory size.
        const hugeDesiredGuestBytes = 0xffff_ffff;
        const jsLayoutHuge = computeGuestRamLayout(hugeDesiredGuestBytes);
        const wasmLayoutHuge = api.guest_ram_layout(hugeDesiredGuestBytes);
        expect(wasmLayoutHuge.guest_base >>> 0).toBe(jsLayoutHuge.guest_base);
        expect(wasmLayoutHuge.guest_size >>> 0).toBe(jsLayoutHuge.guest_size);
        expect(wasmLayoutHuge.runtime_reserved >>> 0).toBe(jsLayoutHuge.runtime_reserved);

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
        const missingWasm =
          variant === "threaded"
            ? message.includes("Missing threaded") && message.includes("WASM package")
            : message.includes("Missing single") && message.includes("WASM package");
        if (missingWasm) {
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
