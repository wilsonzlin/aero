import { describe, expect, it } from "vitest";

import { VirtioInputPciFunction, type VirtioInputPciDeviceLike } from "./virtio_input";

describe("io/devices/virtio_input VirtioInputPciFunction IRQ sync", () => {
  it("raises/lowers the IRQ line based on irq_asserted() during tick()", () => {
    let irq = false;
    let pollCount = 0;
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
      free: () => {},
    };

    const fn = new VirtioInputPciFunction({
      kind: "keyboard",
      device: dev,
      irqSink: {
        raiseIrq: (line) => raised.push(line),
        lowerIrq: (line) => lowered.push(line),
      },
    });

    // Not bus-master enabled by default; tick should not poll the device.
    fn.tick(0);
    expect(pollCount).toBe(0);
    expect(raised).toEqual([]);
    expect(lowered).toEqual([]);

    // Enable PCI Bus Mastering (command bit 2), which allows DMA and thus polling.
    fn.onPciCommandWrite(1 << 2);

    // No IRQ asserted initially.
    fn.tick(1);
    expect(pollCount).toBe(1);
    expect(raised).toEqual([]);
    expect(lowered).toEqual([]);

    // IRQ becomes asserted.
    irq = true;
    fn.tick(2);
    expect(pollCount).toBe(2);
    expect(raised).toEqual([0x05]);
    expect(lowered).toEqual([]);

    // Still asserted: no extra edges.
    fn.tick(3);
    expect(pollCount).toBe(3);
    expect(raised).toEqual([0x05]);
    expect(lowered).toEqual([]);

    // Deasserted.
    irq = false;
    fn.tick(4);
    expect(pollCount).toBe(4);
    expect(raised).toEqual([0x05]);
    expect(lowered).toEqual([0x05]);
  });

  it("syncs the IRQ line on reads from the ISR region (0x2000..0x201f)", () => {
    // Simulate a device that reports IRQ asserted until the guest reads ISR.
    let irq = true;
    const raised: number[] = [];
    const lowered: number[] = [];

    const dev: VirtioInputPciDeviceLike = {
      mmio_read: () => 0,
      mmio_write: () => {},
      poll: () => {},
      driver_ok: () => false,
      irq_asserted: () => irq,
      inject_key: () => {},
      inject_rel: () => {},
      inject_button: () => {},
      inject_wheel: () => {},
      free: () => {},
    };

    const fn = new VirtioInputPciFunction({
      kind: "keyboard",
      device: dev,
      irqSink: {
        raiseIrq: (line) => raised.push(line),
        lowerIrq: (line) => lowered.push(line),
      },
    });

    // A first sync path (e.g. a write/inject) can assert the line.
    fn.tick(0);
    expect(raised).toEqual([0x05]);
    expect(lowered).toEqual([]);

    // Guest reads from ISR; device clears the IRQ afterwards.
    irq = false;
    fn.mmioRead(0, 0x2000n, 1);
    expect(raised).toEqual([0x05]);
    expect(lowered).toEqual([0x05]);
  });

  it("respects PCI command Interrupt Disable bit (bit 10) when syncing INTx level", () => {
    let irq = false;
    const raised: number[] = [];
    const lowered: number[] = [];

    const dev: VirtioInputPciDeviceLike = {
      mmio_read: () => 0,
      mmio_write: () => {},
      poll: () => {},
      driver_ok: () => false,
      irq_asserted: () => irq,
      inject_key: () => {},
      inject_rel: () => {},
      inject_button: () => {},
      inject_wheel: () => {},
      free: () => {},
    };

    const fn = new VirtioInputPciFunction({
      kind: "keyboard",
      device: dev,
      irqSink: {
        raiseIrq: (line) => raised.push(line),
        lowerIrq: (line) => lowered.push(line),
      },
    });

    // Start deasserted.
    fn.tick(0);
    expect(raised).toEqual([]);
    expect(lowered).toEqual([]);

    // Assert line.
    irq = true;
    fn.tick(1);
    expect(raised).toEqual([0x05]);
    expect(lowered).toEqual([]);

    // Disable INTx: should drop the line.
    fn.onPciCommandWrite(1 << 10);
    expect(raised).toEqual([0x05]);
    expect(lowered).toEqual([0x05]);

    // Re-enable: should reassert because the device-level condition is still true.
    fn.onPciCommandWrite(0);
    expect(raised).toEqual([0x05, 0x05]);
    expect(lowered).toEqual([0x05]);
  });
});
