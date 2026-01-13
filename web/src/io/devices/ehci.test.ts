import { describe, expect, it, vi } from "vitest";

import type { IrqSink } from "../device_manager";
import { EhciPciDevice, type EhciControllerBridgeLike } from "./ehci";

describe("io/devices/ehci", () => {
  it("forwards mmioRead/mmioWrite to the underlying bridge and masks writes to access size", () => {
    const bridge: EhciControllerBridgeLike = {
      mmio_read: vi.fn(() => 0x1234_5678),
      mmio_write: vi.fn(),
      step_frames: vi.fn(),
      irq_asserted: vi.fn(() => false),
      free: vi.fn(),
    };
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };

    const dev = new EhciPciDevice({ bridge, irqSink });

    expect(dev.bars).toEqual([{ kind: "mmio32", size: 0x1000 }, null, null, null, null, null]);

    expect(dev.mmioRead(0, 0x04n, 2)).toBe(0x5678);
    expect(bridge.mmio_read).toHaveBeenCalledWith(0x04, 2);

    dev.mmioWrite(0, 0x08n, 2, 0xfeed_beef);
    expect(bridge.mmio_write).toHaveBeenCalledWith(0x08, 2, 0xbeef);
  });

  it("treats PCI INTx as a level-triggered IRQ and only emits transitions on edges", () => {
    let irq = false;
    const bridge: EhciControllerBridgeLike = {
      mmio_read: vi.fn(() => 0),
      mmio_write: vi.fn(),
      step_frames: vi.fn(),
      irq_asserted: vi.fn(() => irq),
      free: vi.fn(),
    };
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };

    const dev = new EhciPciDevice({ bridge, irqSink });

    dev.tick(0);
    dev.tick(1);
    expect(irqSink.raiseIrq).not.toHaveBeenCalled();

    irq = true;
    dev.tick(2);
    expect(irqSink.raiseIrq).toHaveBeenCalledTimes(1);
    expect(irqSink.raiseIrq).toHaveBeenCalledWith(dev.irqLine);

    // No additional edge when irq remains asserted.
    dev.tick(3);
    expect(irqSink.raiseIrq).toHaveBeenCalledTimes(1);
    expect(irqSink.lowerIrq).toHaveBeenCalledTimes(0);

    irq = false;
    dev.tick(4);
    expect(irqSink.lowerIrq).toHaveBeenCalledTimes(1);
    expect(irqSink.lowerIrq).toHaveBeenCalledWith(dev.irqLine);
  });

  it("converts worker ticks into 1ms frames and calls step_frames", () => {
    const bridge: EhciControllerBridgeLike = {
      mmio_read: vi.fn(() => 0),
      mmio_write: vi.fn(),
      step_frames: vi.fn(),
      irq_asserted: vi.fn(() => false),
      free: vi.fn(),
    };
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };

    const dev = new EhciPciDevice({ bridge, irqSink });
    dev.tick(0);
    dev.tick(8);

    expect(bridge.step_frames).toHaveBeenCalledTimes(1);
    expect(bridge.step_frames).toHaveBeenCalledWith(8);
  });
});

