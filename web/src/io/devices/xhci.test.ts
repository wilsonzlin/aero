import { describe, expect, it, vi } from "vitest";

import type { IrqSink } from "../device_manager";
import { XhciPciDevice, type XhciControllerBridgeLike } from "./xhci";

describe("io/devices/xhci", () => {
  it("exposes the expected PCI identity and BAR layout", () => {
    const bridge: XhciControllerBridgeLike = {
      mmio_read: vi.fn(() => 0),
      mmio_write: vi.fn(),
      step_frame: vi.fn(),
      irq_asserted: vi.fn(() => false),
      free: vi.fn(),
    };
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };

    const dev = new XhciPciDevice({ bridge, irqSink });
    expect(dev.bdf).toEqual({ bus: 0, device: 0x0d, function: 0 });
    // Canonical QEMU-style xHCI ("qemu-xhci") PCI identity: 1b36:000d.
    expect(dev.vendorId).toBe(0x1b36);
    expect(dev.deviceId).toBe(0x000d);
    expect(dev.subsystemVendorId).toBe(0x1b36);
    expect(dev.subsystemDeviceId).toBe(0x000d);
    expect(dev.bars).toEqual([{ kind: "mmio32", size: 0x1_0000 }, null, null, null, null, null]);
    expect(dev.classCode).toBe(0x0c0330);
    expect(dev.revisionId).toBe(0x01);
    expect(dev.irqLine).toBe(11);
  });

  it("accepts camelCase xHCI bridge exports (backwards compatibility)", () => {
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };
    const mmioRead = vi.fn(() => 0);
    const mmioWrite = vi.fn();
    const stepFrames = vi.fn();
    const irqAsserted = vi.fn(() => false);
    const setPciCommand = vi.fn();
    const poll = vi.fn();
    const free = vi.fn();

    const bridge = { mmioRead, mmioWrite, stepFrames, irqAsserted, setPciCommand, poll, free };
    const dev = new XhciPciDevice({ bridge: bridge as unknown as XhciControllerBridgeLike, irqSink });

    // Enable MMIO decoding + bus mastering.
    dev.onPciCommandWrite((1 << 1) | (1 << 2));
    expect(setPciCommand).toHaveBeenCalledWith(0x0006);

    dev.mmioRead(0, 0n, 4);
    expect(mmioRead).toHaveBeenCalledWith(0, 4);
    dev.mmioWrite(0, 0n, 4, 0x1234);
    expect(mmioWrite).toHaveBeenCalledWith(0, 4, 0x1234);

    dev.tick(0);
    dev.tick(5);
    expect(stepFrames).toHaveBeenCalledWith(5);
    expect(poll).toHaveBeenCalledTimes(1);

    dev.destroy();
    expect(free).toHaveBeenCalled();
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

  it("prefers step_frames(frames) over tick/step_frame fallbacks when available", () => {
    const bridge: XhciControllerBridgeLike = {
      mmio_read: vi.fn(() => 0),
      mmio_write: vi.fn(),
      step_frames: vi.fn(),
      tick: vi.fn(),
      step_frame: vi.fn(),
      tick_1ms: vi.fn(),
      irq_asserted: vi.fn(() => false),
      free: vi.fn(),
    };
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };
    const dev = new XhciPciDevice({ bridge, irqSink });
    dev.onPciCommandWrite(1 << 2);

    dev.tick(0);
    dev.tick(5);

    expect(bridge.step_frames).toHaveBeenCalledWith(5);
    expect(bridge.tick).not.toHaveBeenCalled();
    expect(bridge.step_frame).not.toHaveBeenCalled();
    expect(bridge.tick_1ms).not.toHaveBeenCalled();
  });

  it("falls back to tick(frames) when step_frames is unavailable", () => {
    const bridge: XhciControllerBridgeLike = {
      mmio_read: vi.fn(() => 0),
      mmio_write: vi.fn(),
      tick: vi.fn(),
      step_frame: vi.fn(),
      tick_1ms: vi.fn(),
      irq_asserted: vi.fn(() => false),
      free: vi.fn(),
    };
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };
    const dev = new XhciPciDevice({ bridge, irqSink });
    dev.onPciCommandWrite(1 << 2);

    dev.tick(0);
    dev.tick(4);

    expect(bridge.tick).toHaveBeenCalledWith(4);
    expect(bridge.step_frame).not.toHaveBeenCalled();
    expect(bridge.tick_1ms).not.toHaveBeenCalled();
  });

  it("falls back to tick_1ms() when only per-frame stepping is available", () => {
    const bridge: XhciControllerBridgeLike = {
      mmio_read: vi.fn(() => 0),
      mmio_write: vi.fn(),
      tick_1ms: vi.fn(),
      irq_asserted: vi.fn(() => false),
      free: vi.fn(),
    };
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };
    const dev = new XhciPciDevice({ bridge, irqSink });
    dev.onPciCommandWrite(1 << 2);

    dev.tick(0);
    dev.tick(3);

    expect(bridge.tick_1ms).toHaveBeenCalledTimes(3);
  });

  it("mirrors PCI command writes into the bridge when set_pci_command is implemented", () => {
    const bridge: XhciControllerBridgeLike = {
      mmio_read: vi.fn(() => 0),
      mmio_write: vi.fn(),
      step_frame: vi.fn(),
      irq_asserted: vi.fn(() => false),
      set_pci_command: vi.fn(),
      free: vi.fn(),
    };
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };
    const dev = new XhciPciDevice({ bridge, irqSink });

    dev.onPciCommandWrite(0x1_2345);
    expect(bridge.set_pci_command).toHaveBeenCalledWith(0x2345);
  });

  it("calls poll() when available even if no full USB frame elapsed", () => {
    const bridge: XhciControllerBridgeLike = {
      mmio_read: vi.fn(() => 0),
      mmio_write: vi.fn(),
      tick_1ms: vi.fn(),
      poll: vi.fn(),
      irq_asserted: vi.fn(() => false),
      free: vi.fn(),
    };
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };
    const dev = new XhciPciDevice({ bridge, irqSink });
    dev.onPciCommandWrite(1 << 2);

    // First tick initializes the time base.
    dev.tick(0);
    // Less than 1ms elapsed; no frames should be stepped, but poll should still run.
    dev.tick(0.5);

    expect(bridge.tick_1ms).not.toHaveBeenCalled();
    expect(bridge.poll).toHaveBeenCalledTimes(1);
  });

  it("gates DMA stepping on PCI Bus Master Enable (command bit 2) when set_pci_command is unavailable", () => {
    const bridge: XhciControllerBridgeLike = {
      mmio_read: vi.fn(() => 0),
      mmio_write: vi.fn(),
      step_frame: vi.fn(),
      poll: vi.fn(),
      irq_asserted: vi.fn(() => false),
      free: vi.fn(),
    };
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };
    const dev = new XhciPciDevice({ bridge, irqSink });

    dev.tick(0);
    dev.tick(8);
    expect(bridge.step_frame).not.toHaveBeenCalled();
    expect(bridge.poll).not.toHaveBeenCalled();

    // Enable bus mastering; the device should start stepping from "now" without catching up.
    dev.onPciCommandWrite(1 << 2);
    dev.tick(9);
    expect(bridge.step_frame).toHaveBeenCalledTimes(1);
    expect(bridge.poll).toHaveBeenCalledTimes(1);
  });

  it("advances time while BME is off when the bridge supports set_pci_command", () => {
    const bridge: XhciControllerBridgeLike = {
      mmio_read: vi.fn(() => 0),
      mmio_write: vi.fn(),
      step_frame: vi.fn(),
      poll: vi.fn(),
      irq_asserted: vi.fn(() => false),
      set_pci_command: vi.fn(),
      free: vi.fn(),
    };
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };
    const dev = new XhciPciDevice({ bridge, irqSink });

    dev.tick(0);
    dev.tick(8);
    // Even with bus mastering disabled, internal controller time should advance. `poll()` is
    // suppressed because it may perform DMA.
    expect(bridge.step_frame).toHaveBeenCalledTimes(8);
    expect(bridge.poll).not.toHaveBeenCalled();

    dev.onPciCommandWrite(1 << 2);
    dev.tick(9);
    expect(bridge.step_frame).toHaveBeenCalledTimes(9);
    expect(bridge.poll).toHaveBeenCalledTimes(1);
  });
});
