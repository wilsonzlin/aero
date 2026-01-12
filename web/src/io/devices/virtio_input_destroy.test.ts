import { describe, expect, it } from "vitest";

import { VirtioInputPciFunction, type VirtioInputPciDeviceLike } from "./virtio_input";

describe("io/devices/virtio_input VirtioInputPciFunction.destroy", () => {
  it("lowers an asserted IRQ and frees the underlying WASM device exactly once", () => {
    let irq = true;
    let pollCount = 0;
    let freeCount = 0;
    const raised: number[] = [];
    const lowered: number[] = [];

    const dev: VirtioInputPciDeviceLike = {
      mmio_read: () => 0,
      mmio_write: () => {},
      poll: () => {
        pollCount += 1;
      },
      driver_ok: () => false,
      irq_asserted: () => irq,
      inject_key: () => {},
      inject_rel: () => {},
      inject_button: () => {},
      inject_wheel: () => {},
      free: () => {
        freeCount += 1;
      },
    };

    const fn = new VirtioInputPciFunction({
      kind: "keyboard",
      device: dev,
      irqSink: {
        raiseIrq: (line) => raised.push(line),
        lowerIrq: (line) => lowered.push(line),
      },
    });

    // Allow polling/DMA via PCI Bus Master Enable (command bit 2).
    fn.onPciCommandWrite(1 << 2);

    // Establish IRQ asserted state.
    fn.tick(0);
    expect(pollCount).toBe(1);
    expect(raised).toEqual([0x05]);
    expect(lowered).toEqual([]);

    // Destroy should drop the line and free the device.
    fn.destroy();
    expect(freeCount).toBe(1);
    expect(lowered).toEqual([0x05]);

    // Subsequent destroys should be no-ops (no double-free / no extra IRQ edges).
    fn.destroy();
    expect(freeCount).toBe(1);
    expect(lowered).toEqual([0x05]);

    // After destroy, ticks should not poll or touch IRQ.
    irq = false;
    fn.tick(1);
    expect(pollCount).toBe(1);
    expect(raised).toEqual([0x05]);
    expect(lowered).toEqual([0x05]);
  });
});
