import { describe, expect, it, vi } from "vitest";
import { UhciWebUsbPciDevice } from "./uhci_webusb";
import { applyUsbSelectedToWebUsbUhciBridge, type WebUsbUhciHotplugBridgeLike } from "../../usb/uhci_webusb_bridge";
import type { WebUsbUhciBridgeLike } from "./uhci_webusb";

describe("UhciWebUsbPciDevice", () => {
  it("forwards BAR4 I/O reads/writes to the WASM bridge with the same offset/size", () => {
    const io_read = vi.fn(() => 0x1234_5678);
    const io_write = vi.fn();
    const step_frames = vi.fn();
    const irq_level = vi.fn(() => false);
    const free = vi.fn();

    const dev = new UhciWebUsbPciDevice({
      bridge: { io_read, io_write, step_frames, irq_level, free },
      irqSink: { raiseIrq: vi.fn(), lowerIrq: vi.fn() },
    });

    const v = dev.ioRead?.(4, 0x10, 2);
    expect(v).toBe(0x5678);
    expect(io_read).toHaveBeenCalledWith(0x10, 2);

    dev.ioWrite?.(4, 0x08, 4, 0xdead_beef);
    expect(io_write).toHaveBeenCalledWith(0x08, 4, 0xdead_beef);
  });

  it("accepts camelCase WebUsbUhciBridge exports (backwards compatibility)", () => {
    const irqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };
    const ioRead = vi.fn(() => 0);
    const ioWrite = vi.fn();
    const stepFrames = vi.fn();
    const irqLevel = vi.fn(() => false);
    const setPciCommand = vi.fn();
    const free = vi.fn();

    const bridge = { ioRead, ioWrite, stepFrames, irqLevel, setPciCommand, free };
    const dev = new UhciWebUsbPciDevice({ bridge: bridge as unknown as WebUsbUhciBridgeLike, irqSink });

    dev.ioRead?.(4, 0x10, 2);
    expect(ioRead).toHaveBeenCalledWith(0x10, 2);
    dev.ioWrite?.(4, 0x08, 4, 0xdead_beef);
    expect(ioWrite).toHaveBeenCalledWith(0x08, 4, 0xdead_beef);

    // Enable bus mastering and ensure the PCI command is mirrored.
    dev.onPciCommandWrite?.(0x1_0004);
    expect(setPciCommand).toHaveBeenCalledWith(0x0004);

    dev.tick(0);
    dev.tick(8);
    expect(stepFrames).toHaveBeenCalledWith(8);

    dev.destroy();
    expect(free).toHaveBeenCalled();
  });

  it("calls bridge.free() on destroy() exactly once and is idempotent", () => {
    const free = vi.fn();
    const dev = new UhciWebUsbPciDevice({
      bridge: {
        io_read: vi.fn(() => 0),
        io_write: vi.fn(),
        step_frames: vi.fn(),
        irq_level: vi.fn(() => false),
        free,
      },
      irqSink: { raiseIrq: vi.fn(), lowerIrq: vi.fn() },
    });

    dev.destroy();
    dev.destroy();
    expect(free).toHaveBeenCalledTimes(1);
  });

  it("treats PCI INTx as a level-triggered IRQ and only emits transitions on edges", () => {
    let level = false;
    const irq_level = vi.fn(() => level);
    const step_frames = vi.fn(() => {
      level = true;
    });

    const raiseIrq = vi.fn();
    const lowerIrq = vi.fn();

    const dev = new UhciWebUsbPciDevice({
      bridge: {
        io_read: vi.fn(() => 0),
        io_write: vi.fn(),
        step_frames,
        irq_level,
        free: vi.fn(),
      },
      irqSink: { raiseIrq, lowerIrq },
    });
    // Allow the controller to DMA into guest memory.
    dev.onPciCommandWrite?.(1 << 2);

    dev.tick(1000);
    expect(step_frames).not.toHaveBeenCalled();
    expect(raiseIrq).not.toHaveBeenCalled();

    dev.tick(1008);
    expect(step_frames).toHaveBeenCalledWith(8);
    expect(raiseIrq).toHaveBeenCalledWith(dev.irqLine);
    expect(lowerIrq).not.toHaveBeenCalled();

    // No additional edge while asserted.
    dev.tick(1016);
    expect(raiseIrq).toHaveBeenCalledTimes(1);
    expect(lowerIrq).not.toHaveBeenCalled();

    level = false;
    // Use a 0ms delta so step_frames() doesn't override our manual level flip.
    dev.tick(1016);
    expect(lowerIrq).toHaveBeenCalledTimes(1);
    expect(lowerIrq).toHaveBeenCalledWith(dev.irqLine);
  });

  it("gates DMA stepping on PCI Bus Master Enable (command bit 2) when set_pci_command is unavailable", () => {
    const step_frames = vi.fn();
    const dev = new UhciWebUsbPciDevice({
      bridge: {
        io_read: vi.fn(() => 0),
        io_write: vi.fn(),
        step_frames,
        irq_level: vi.fn(() => false),
        free: vi.fn(),
      },
      irqSink: { raiseIrq: vi.fn(), lowerIrq: vi.fn() },
    });

    dev.tick(0);
    dev.tick(8);
    expect(step_frames).not.toHaveBeenCalled();

    // Enable bus mastering; the device should start stepping from "now" without catching up.
    dev.onPciCommandWrite?.(1 << 2);
    dev.tick(9);
    expect(step_frames).toHaveBeenCalledTimes(1);
    expect(step_frames).toHaveBeenCalledWith(1);
  });

  it("advances time while BME is off when the bridge supports set_pci_command", () => {
    const step_frames = vi.fn();
    const dev = new UhciWebUsbPciDevice({
      bridge: {
        io_read: vi.fn(() => 0),
        io_write: vi.fn(),
        step_frames,
        irq_level: vi.fn(() => false),
        set_pci_command: vi.fn(),
        free: vi.fn(),
      },
      irqSink: { raiseIrq: vi.fn(), lowerIrq: vi.fn() },
    });

    dev.tick(0);
    dev.tick(8);
    expect(step_frames).toHaveBeenCalledTimes(1);
    expect(step_frames).toHaveBeenCalledWith(8);

    dev.onPciCommandWrite?.(1 << 2);
    dev.tick(9);
    expect(step_frames).toHaveBeenCalledTimes(2);
    expect(step_frames).toHaveBeenLastCalledWith(1);
  });

  it("suppresses INTx assertion when PCI command INTX_DISABLE bit is set", () => {
    const raiseIrq = vi.fn();
    const dev = new UhciWebUsbPciDevice({
      bridge: {
        io_read: vi.fn(() => 0),
        io_write: vi.fn(),
        step_frames: vi.fn(),
        irq_level: vi.fn(() => true),
        free: vi.fn(),
      },
      irqSink: { raiseIrq, lowerIrq: vi.fn() },
    });

    // Bus mastering enabled but INTx disabled.
    dev.onPciCommandWrite?.((1 << 2) | (1 << 10));
    dev.tick(0);
    dev.tick(1);
    expect(raiseIrq).not.toHaveBeenCalled();

    // Re-enable INTx: pending asserted level should become visible.
    dev.onPciCommandWrite?.(1 << 2);
    dev.tick(2);
    expect(raiseIrq).toHaveBeenCalledWith(dev.irqLine);
  });

  it("mirrors 16-bit PCI command writes into the WASM bridge when set_pci_command is present", () => {
    const set_pci_command = vi.fn();
    const dev = new UhciWebUsbPciDevice({
      bridge: {
        io_read: vi.fn(() => 0),
        io_write: vi.fn(),
        step_frames: vi.fn(),
        irq_level: vi.fn(() => false),
        set_pci_command,
        free: vi.fn(),
      },
      irqSink: { raiseIrq: vi.fn(), lowerIrq: vi.fn() },
    });

    dev.onPciCommandWrite?.(0x1_0000 | (1 << 2));
    expect(set_pci_command).toHaveBeenCalledWith(1 << 2);
  });
});

describe("applyUsbSelectedToWebUsbUhciBridge", () => {
  it("connects on ok:true and disconnects+resets on ok:false", () => {
    const bridge = {
      set_connected: vi.fn(),
      reset: vi.fn(),
    };

    applyUsbSelectedToWebUsbUhciBridge(bridge, {
      type: "usb.selected",
      ok: true,
      info: { vendorId: 0x1234, productId: 0x5678 },
    });
    expect(bridge.set_connected).toHaveBeenCalledWith(true);
    expect(bridge.reset).not.toHaveBeenCalled();

    bridge.set_connected.mockClear();
    bridge.reset.mockClear();

    applyUsbSelectedToWebUsbUhciBridge(bridge, { type: "usb.selected", ok: false, error: "no device" });
    expect(bridge.set_connected).toHaveBeenCalledWith(false);
    expect(bridge.reset).toHaveBeenCalled();
  });

  it("accepts camelCase setConnected() (backwards compatibility)", () => {
    const bridge = {
      setConnected: vi.fn(),
      reset: vi.fn(),
    };

    applyUsbSelectedToWebUsbUhciBridge(bridge as unknown as WebUsbUhciHotplugBridgeLike, {
      type: "usb.selected",
      ok: true,
      info: { vendorId: 0x1234, productId: 0x5678 },
    });
    expect(bridge.setConnected).toHaveBeenCalledWith(true);
    expect(bridge.reset).not.toHaveBeenCalled();

    bridge.setConnected.mockClear();
    bridge.reset.mockClear();

    applyUsbSelectedToWebUsbUhciBridge(bridge as unknown as WebUsbUhciHotplugBridgeLike, {
      type: "usb.selected",
      ok: false,
      error: "no device",
    });
    expect(bridge.setConnected).toHaveBeenCalledWith(false);
    expect(bridge.reset).toHaveBeenCalled();
  });
});
