import { describe, expect, it } from "vitest";

import { initWasmForContext } from "../../runtime/wasm_context";
import { assertWasmMemoryWiring } from "../../runtime/wasm_memory_probe";

// Keep the test self-contained: allocate a minimal non-shared wasm memory so the module has room
// for its runtime heap (the allocator reserves a fixed low region for the runtime).
function makeTestMemory(): WebAssembly.Memory {
  const pageBytes = 64 * 1024;
  const reservedBytes = 128 * 1024 * 1024; // keep in sync with `RUNTIME_RESERVED_BYTES`
  const pages = Math.ceil(reservedBytes / pageBytes);
  return new WebAssembly.Memory({ initial: pages, maximum: pages });
}

describe("I8042Bridge (wasm)", () => {
  it("injects keyboard + mouse input and exposes IRQ pulses via drain_irqs()", async () => {
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

    assertWasmMemoryWiring({ api, memory, context: "I8042Bridge(wasm) test" });
    if (!api.I8042Bridge) {
      throw new Error("I8042Bridge wasm export is unavailable; rebuild web/src/wasm/pkg-single.");
    }

    const dev = new api.I8042Bridge();
    const drainIrqs = (() => {
      const anyDev = dev as unknown as { drain_irqs?: unknown };
      if (typeof anyDev.drain_irqs !== "function") {
        throw new Error("I8042Bridge.drain_irqs() is unavailable; rebuild web/src/wasm/pkg-single.");
      }
      return () => (anyDev.drain_irqs as (...args: unknown[]) => unknown).call(dev) as number;
    })();
    try {
      // No pending IRQ pulses initially.
      expect(drainIrqs() & 0x03).toBe(0);

      // Inject Set-2 make code for 'A' (0x1C). Default command byte enables Set-2->Set-1
      // translation, so the guest should observe Set-1 scancode 0x1E.
      dev.inject_key_scancode_bytes(0x1c, 1);
      expect(drainIrqs() & 0x01).toBe(0x01);

      const statusBefore = dev.port_read(0x64);
      expect(statusBefore & 0x01).toBe(0x01); // OBF
      expect(statusBefore & 0x20).toBe(0x00); // AUX clear (keyboard)

      const byte = dev.port_read(0x60);
      expect(byte).toBe(0x1e);
      expect(drainIrqs() & 0x01).toBe(0);

      // Regression test: when the output buffer refills immediately after a data port read, the
      // i8042 should generate another IRQ pulse for the newly available byte.
      //
      // With a level-only `irq_mask()` API, this pulse can be missed because the mask stays
      // asserted across the entire read+refill sequence.
      dev.inject_key_scancode_bytes(0x1c, 1);
      expect(drainIrqs() & 0x01).toBe(0x01);
      // Queue another key while the output buffer is still full; no new pulse yet.
      dev.inject_key_scancode_bytes(0x32, 1);
      expect(drainIrqs() & 0x01).toBe(0);
      // Reading the first byte should refill and generate another pulse.
      dev.port_read(0x60);
      expect(drainIrqs() & 0x01).toBe(0x01);
      // Reading the final byte should not generate any more pulses.
      dev.port_read(0x60);
      expect(drainIrqs() & 0x01).toBe(0);

      // Enable IRQ12 (bit 1) while keeping translation + IRQ1 enabled (default 0x45 -> 0x47).
      dev.port_write(0x64, 0x60);
      dev.port_write(0x60, 0x47);

      // Enable mouse reporting (send 0xF4 to the mouse via command 0xD4).
      dev.port_write(0x64, 0xd4);
      dev.port_write(0x60, 0xf4);
      // Drain the mouse ACK (0xFA).
      while ((dev.port_read(0x64) & 0x01) !== 0) {
        dev.port_read(0x60);
        // ACK bytes may also trigger IRQ pulses; drain them so subsequent assertions are stable.
        drainIrqs();
      }
      drainIrqs();

      // Inject a small motion packet (dx=5 right, dy=3 up).
      dev.inject_mouse_move(5, 3);
      expect(drainIrqs() & 0x02).toBe(0x02);

      const statusMouse = dev.port_read(0x64);
      expect(statusMouse & 0x01).toBe(0x01); // OBF
      expect(statusMouse & 0x20).toBe(0x20); // AUX set (mouse)

      const b0 = dev.port_read(0x60);
      const b1 = dev.port_read(0x60);
      const b2 = dev.port_read(0x60);
      expect(b0).toBe(0x08);
      expect(b1).toBe(5);
      expect(b2).toBe(3);

      // Snapshot/restore should keep the internal mouse button image in sync with the host-side
      // button-mask injector. Regression: if the bridge resets its internal tracking to 0 on
      // restore, a queued button-release (mask=0) after restore may become a no-op and leave the
      // guest with a stuck mouse button.
      dev.inject_mouse_buttons(0x01);
      // Drain the button packet.
      while ((dev.port_read(0x64) & 0x01) !== 0) dev.port_read(0x60);
      drainIrqs();
      const snap = dev.save_state();
      dev.inject_mouse_buttons(0x00);
      while ((dev.port_read(0x64) & 0x01) !== 0) dev.port_read(0x60);
      drainIrqs();

      dev.load_state(snap);
      // Try to release after restore.
      dev.inject_mouse_buttons(0x00);
      dev.inject_mouse_move(0, 0);
      const s0 = dev.port_read(0x60);
      // bit0 is left button; should be released.
      expect(s0 & 0x01).toBe(0);
    } finally {
      dev.free();
    }
  });
});
