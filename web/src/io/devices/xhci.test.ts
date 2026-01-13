import { describe, expect, it, vi } from "vitest";

import type { IrqSink } from "../device_manager";
import { XhciPciDevice, type XhciControllerBridgeLike } from "./xhci";

describe("io/devices/xhci", () => {
  it("exposes an xHCI MMIO BAR (BAR0) and correct class code", () => {
    const bridge: XhciControllerBridgeLike = {
      mmio_read: vi.fn(() => 0),
      mmio_write: vi.fn(),
      step_frame: vi.fn(),
      irq_asserted: vi.fn(() => false),
      free: vi.fn(),
    };
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };

    const dev = new XhciPciDevice({ bridge, irqSink });
    expect(dev.bars).toEqual([{ kind: "mmio32", size: 0x1_0000 }, null, null, null, null, null]);
    expect(dev.classCode).toBe(0x0c0330);
    expect(dev.irqLine).toBe(11);
  });

  it("forwards mmioRead/mmioWrite to the underlying bridge when PCI MEM is enabled", () => {
    const bridge: XhciControllerBridgeLike = {
      mmio_read: vi.fn(() => 0x1234_5678),
      mmio_write: vi.fn(),
      step_frame: vi.fn(),
      irq_asserted: vi.fn(() => false),
      free: vi.fn(),
    };
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };
    const dev = new XhciPciDevice({ bridge, irqSink });

    // Enable Memory Space Enable (bit 1) so MMIO is decoded.
    dev.onPciCommandWrite(1 << 1);

    expect(dev.mmioRead(0, 0x04n, 2)).toBe(0x5678);
    expect(bridge.mmio_read).toHaveBeenCalledWith(0x04, 2);

    dev.mmioWrite(0, 0x06n, 2, 0xfeed_beef);
    expect(bridge.mmio_write).toHaveBeenCalledWith(0x06, 2, 0xbeef);
  });

  it("gates MMIO on PCI Memory Space Enable (command bit 1)", () => {
    const bridge: XhciControllerBridgeLike = {
      mmio_read: vi.fn(() => 0x1234_5678),
      mmio_write: vi.fn(),
      step_frame: vi.fn(),
      irq_asserted: vi.fn(() => false),
      free: vi.fn(),
    };
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };
    const dev = new XhciPciDevice({ bridge, irqSink });

    // MMIO is disabled by default: reads return unmapped value and do not hit the bridge.
    expect(dev.mmioRead(0, 0x00n, 4)).toBe(0xffff_ffff);
    expect(bridge.mmio_read).not.toHaveBeenCalled();

    dev.mmioWrite(0, 0x00n, 4, 0);
    expect(bridge.mmio_write).not.toHaveBeenCalled();
  });

  it("treats PCI INTx as a level-triggered IRQ and only emits transitions on edges", () => {
    let irq = false;
    const bridge: XhciControllerBridgeLike = {
      mmio_read: vi.fn(() => 0),
      mmio_write: vi.fn(),
      step_frame: vi.fn(),
      irq_asserted: vi.fn(() => irq),
      free: vi.fn(),
    };
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };
    const dev = new XhciPciDevice({ bridge, irqSink });

    // Allow the controller to DMA into guest memory.
    dev.onPciCommandWrite(1 << 2);

    dev.tick(0);
    dev.tick(8);
    expect(bridge.step_frame).toHaveBeenCalledTimes(8);
    expect(irqSink.raiseIrq).not.toHaveBeenCalled();

    irq = true;
    dev.tick(9);
    expect(bridge.step_frame).toHaveBeenCalledTimes(9);
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

  it("suppresses INTx assertion when PCI command INTX_DISABLE bit is set", () => {
    const bridge: XhciControllerBridgeLike = {
      mmio_read: vi.fn(() => 0),
      mmio_write: vi.fn(),
      step_frame: vi.fn(),
      irq_asserted: vi.fn(() => true),
      free: vi.fn(),
    };
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };
    const dev = new XhciPciDevice({ bridge, irqSink });

    // Bus mastering enabled but INTx disabled.
    dev.onPciCommandWrite((1 << 2) | (1 << 10));
    dev.tick(0);
    dev.tick(1);
    expect(irqSink.raiseIrq).not.toHaveBeenCalled();

    // Re-enable INTx: pending asserted level should become visible.
    dev.onPciCommandWrite(1 << 2);
    dev.tick(2);
    expect(irqSink.raiseIrq).toHaveBeenCalledWith(11);
  });

  it("gates DMA stepping on PCI Bus Master Enable (command bit 2)", () => {
    const bridge: XhciControllerBridgeLike = {
      mmio_read: vi.fn(() => 0),
      mmio_write: vi.fn(),
      step_frame: vi.fn(),
      irq_asserted: vi.fn(() => false),
      free: vi.fn(),
    };
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };
    const dev = new XhciPciDevice({ bridge, irqSink });

    dev.tick(0);
    dev.tick(8);
    expect(bridge.step_frame).not.toHaveBeenCalled();

    // Enable bus mastering; the device should start stepping from "now" without catching up.
    dev.onPciCommandWrite(1 << 2);
    dev.tick(9);
    expect(bridge.step_frame).toHaveBeenCalledTimes(1);
  });
});

