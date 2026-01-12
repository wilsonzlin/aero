import { describe, expect, it } from "vitest";

import { initWasmForContext } from "../../runtime/wasm_context";

// Keep the test self-contained: allocate a minimal non-shared wasm memory so the module has room
// for its runtime heap (the allocator reserves a fixed low region for the runtime).
function makeTestMemory(): WebAssembly.Memory {
  const pageBytes = 64 * 1024;
  const reservedBytes = 128 * 1024 * 1024; // keep in sync with `RUNTIME_RESERVED_BYTES`
  const pages = Math.ceil(reservedBytes / pageBytes);
  return new WebAssembly.Memory({ initial: pages, maximum: pages });
}

describe("I8042Bridge (wasm)", () => {
  it("injects keyboard + mouse input and exposes IRQ levels via irq_mask()", async () => {
    const memory = makeTestMemory();
    let api: Awaited<ReturnType<typeof initWasmForContext>>["api"];
    try {
      ({ api } = await initWasmForContext({ variant: "single", memory }));
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      // `npm test` does not build the WASM packages by default. Skip this test when the
      // `wasm-pack` output is absent (fresh checkout / CI without `npm run wasm:build`).
      if (message.includes("Missing single-thread WASM package")) {
        return;
      }
      throw err;
    }
    if (!api.I8042Bridge) {
      throw new Error("I8042Bridge wasm export is unavailable; rebuild web/src/wasm/pkg-single.");
    }

    const dev = new api.I8042Bridge();
    try {
      // No pending output/irqs initially.
      expect(dev.irq_mask() & 0x03).toBe(0);

      // Inject Set-2 make code for 'A' (0x1C). Default command byte enables Set-2->Set-1
      // translation, so the guest should observe Set-1 scancode 0x1E.
      dev.inject_key_scancode_bytes(0x1c, 1);
      expect(dev.irq_mask() & 0x01).toBe(0x01);

      const statusBefore = dev.port_read(0x64);
      expect(statusBefore & 0x01).toBe(0x01); // OBF
      expect(statusBefore & 0x20).toBe(0x00); // AUX clear (keyboard)

      const byte = dev.port_read(0x60);
      expect(byte).toBe(0x1e);
      expect(dev.irq_mask() & 0x01).toBe(0);

      // Enable IRQ12 (bit 1) while keeping translation + IRQ1 enabled (default 0x45 -> 0x47).
      dev.port_write(0x64, 0x60);
      dev.port_write(0x60, 0x47);

      // Enable mouse reporting (send 0xF4 to the mouse via command 0xD4).
      dev.port_write(0x64, 0xd4);
      dev.port_write(0x60, 0xf4);
      // Drain the mouse ACK (0xFA).
      while ((dev.port_read(0x64) & 0x01) !== 0) {
        dev.port_read(0x60);
      }

      // Inject a small motion packet (dx=5 right, dy=3 up).
      dev.inject_mouse_move(5, 3);
      expect(dev.irq_mask() & 0x02).toBe(0x02);

      const statusMouse = dev.port_read(0x64);
      expect(statusMouse & 0x01).toBe(0x01); // OBF
      expect(statusMouse & 0x20).toBe(0x20); // AUX set (mouse)

      const b0 = dev.port_read(0x60);
      const b1 = dev.port_read(0x60);
      const b2 = dev.port_read(0x60);
      expect(b0).toBe(0x08);
      expect(b1).toBe(5);
      expect(b2).toBe(3);
    } finally {
      dev.free();
    }
  });
});
