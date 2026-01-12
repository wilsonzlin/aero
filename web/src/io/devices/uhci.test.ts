import { describe, expect, it, vi } from "vitest";

import type { IrqSink } from "../device_manager";
import { UhciPciDevice, type UhciControllerBridgeLike } from "./uhci";

describe("io/devices/UhciPciDevice", () => {
  it("exposes a UHCI IO BAR (BAR4) sized for the register block", () => {
    const bridge: UhciControllerBridgeLike = {
      io_read: vi.fn(() => 0),
      io_write: vi.fn(),
      tick_1ms: vi.fn(),
      irq_asserted: vi.fn(() => false),
      free: vi.fn(),
    };
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };

    const dev = new UhciPciDevice({ bridge, irqSink });
    expect(dev.bdf).toEqual({ bus: 0, device: 1, function: 0 });
    expect(dev.bars).toEqual([null, null, null, null, { kind: "io", size: 0x20 }, null]);
    expect(dev.classCode).toBe(0x0c0300);
    expect(dev.irqLine).toBe(11);
  });

  it("forwards ioRead/ioWrite to the underlying bridge", () => {
    const bridge: UhciControllerBridgeLike = {
      io_read: vi.fn(() => 0x1234_5678),
      io_write: vi.fn(),
      tick_1ms: vi.fn(),
      irq_asserted: vi.fn(() => false),
      free: vi.fn(),
    };
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };

    const dev = new UhciPciDevice({ bridge, irqSink });

    expect(dev.ioRead(4, 0x04, 2)).toBe(0x5678);
    expect(bridge.io_read).toHaveBeenCalledWith(0x04, 2);

    dev.ioWrite(4, 0x06, 2, 0xfeed_beef);
    expect(bridge.io_write).toHaveBeenCalledWith(0x06, 2, 0xbeef);
  });

  it("treats PCI INTx as a level-triggered IRQ and only emits transitions on edges", () => {
    let irq = false;
    const bridge: UhciControllerBridgeLike = {
      io_read: vi.fn(() => 0),
      io_write: vi.fn(),
      tick_1ms: vi.fn(),
      irq_asserted: vi.fn(() => irq),
      free: vi.fn(),
    };
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };

    const dev = new UhciPciDevice({ bridge, irqSink });
    // Allow the controller to DMA into guest memory.
    dev.onPciCommandWrite(1 << 2);

    dev.tick(0);
    dev.tick(8);
    expect(bridge.tick_1ms).toHaveBeenCalledTimes(8);
    expect(irqSink.raiseIrq).not.toHaveBeenCalled();

    irq = true;
    dev.tick(9);
    expect(bridge.tick_1ms).toHaveBeenCalledTimes(9);
    expect(irqSink.raiseIrq).toHaveBeenCalledTimes(1);
    expect(irqSink.raiseIrq).toHaveBeenCalledWith(11);

    // No additional edge when irq remains asserted.
    dev.tick(10);
    expect(irqSink.raiseIrq).toHaveBeenCalledTimes(1);
    expect(irqSink.lowerIrq).toHaveBeenCalledTimes(0);

    irq = false;
    dev.tick(11);
    expect(irqSink.lowerIrq).toHaveBeenCalledTimes(1);
    expect(irqSink.lowerIrq).toHaveBeenCalledWith(11);
  });

  it("bounds catch-up work in tick()", () => {
    const bridge: UhciControllerBridgeLike = {
      io_read: vi.fn(() => 0),
      io_write: vi.fn(),
      tick_1ms: vi.fn(),
      irq_asserted: vi.fn(() => false),
      free: vi.fn(),
    };
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };

    const dev = new UhciPciDevice({ bridge, irqSink });
    // Allow the controller to DMA into guest memory.
    dev.onPciCommandWrite(1 << 2);
    dev.tick(0);
    dev.tick(10_000);

    // Clamp to 32 frames to avoid large stalls.
    expect(bridge.tick_1ms).toHaveBeenCalledTimes(32);
  });

  it("prefers step_frames() when available", () => {
    const bridge: UhciControllerBridgeLike = {
      io_read: vi.fn(() => 0),
      io_write: vi.fn(),
      step_frames: vi.fn(),
      tick_1ms: vi.fn(),
      irq_asserted: vi.fn(() => false),
      free: vi.fn(),
    };
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };

    const dev = new UhciPciDevice({ bridge, irqSink });
    // Allow the controller to DMA into guest memory.
    dev.onPciCommandWrite(1 << 2);
    dev.tick(0);
    dev.tick(8);

    expect(bridge.step_frames).toHaveBeenCalledTimes(1);
    expect(bridge.step_frames).toHaveBeenCalledWith(8);
    expect(bridge.tick_1ms).not.toHaveBeenCalled();
  });

  it("falls back to step_frame() when tick_1ms() is unavailable", () => {
    const bridge: UhciControllerBridgeLike = {
      io_read: vi.fn(() => 0),
      io_write: vi.fn(),
      step_frame: vi.fn(),
      irq_asserted: vi.fn(() => false),
      free: vi.fn(),
    };
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };

    const dev = new UhciPciDevice({ bridge, irqSink });
    // Allow the controller to DMA into guest memory.
    dev.onPciCommandWrite(1 << 2);
    dev.tick(0);
    dev.tick(3);

    expect(bridge.step_frame).toHaveBeenCalledTimes(3);
  });
});
