import { describe, expect, it, vi } from "vitest";

import { DeviceManager } from "../io/device_manager";
import { VirtioInputPciFunction, VIRTIO_INPUT_PCI_DEVICE, type VirtioInputPciDeviceLike } from "../io/devices/virtio_input";

import { registerVirtioInputKeyboardPciFunction } from "./io_virtio_input_register";

const dummyVirtioInputDevice: VirtioInputPciDeviceLike = {
  mmio_read: () => 0,
  mmio_write: () => {},
  poll: () => {},
  driver_ok: () => false,
  irq_asserted: () => false,
  inject_key: () => {},
  inject_rel: () => {},
  inject_button: () => {},
  inject_wheel: () => {},
  free: () => {},
};

describe("workers/io_virtio_input_register", () => {
  it("registers virtio-input keyboard+mouse at the canonical multifunction BDF when available", () => {
    const mgr = new DeviceManager({ raiseIrq: () => {}, lowerIrq: () => {} });
    const irqSink = mgr.irqSink;

    const keyboardFn = new VirtioInputPciFunction({ kind: "keyboard", device: dummyVirtioInputDevice, irqSink });
    const mouseFn = new VirtioInputPciFunction({ kind: "mouse", device: dummyVirtioInputDevice, irqSink });

    const warn = vi.fn();
    const { addr: keyboardAddr, usedCanonical } = registerVirtioInputKeyboardPciFunction({ mgr, keyboardFn, log: { warn } });
    expect(usedCanonical).toBe(true);
    expect(keyboardAddr).toEqual({ bus: 0, device: VIRTIO_INPUT_PCI_DEVICE, function: 0 });
    expect(warn).not.toHaveBeenCalled();

    const mouseAddr = mgr.registerPciDevice(mouseFn, { device: keyboardAddr.device, function: 1 });
    expect(mouseAddr).toEqual({ bus: 0, device: VIRTIO_INPUT_PCI_DEVICE, function: 1 });
    expect(mouseAddr.device).toBe(keyboardAddr.device);
  });

  it("falls back to auto allocation when the canonical BDF is already in use, while keeping a single device number", () => {
    const mgr = new DeviceManager({ raiseIrq: () => {}, lowerIrq: () => {} });
    const irqSink = mgr.irqSink;

    // Occupy the canonical keyboard function (0:10.0) so virtio-input must fall back.
    mgr.registerPciDevice({ name: "blocker", vendorId: 0x1111, deviceId: 0x2222, classCode: 0 }, { device: VIRTIO_INPUT_PCI_DEVICE, function: 0 });

    const keyboardFn = new VirtioInputPciFunction({ kind: "keyboard", device: dummyVirtioInputDevice, irqSink });
    const mouseFn = new VirtioInputPciFunction({ kind: "mouse", device: dummyVirtioInputDevice, irqSink });

    const warn = vi.fn();
    const { addr: keyboardAddr, usedCanonical } = registerVirtioInputKeyboardPciFunction({ mgr, keyboardFn, log: { warn } });
    expect(usedCanonical).toBe(false);
    expect(warn).toHaveBeenCalledTimes(1);

    expect(keyboardAddr.function).toBe(0);
    expect(keyboardAddr.device).not.toBe(VIRTIO_INPUT_PCI_DEVICE);

    const mouseAddr = mgr.registerPciDevice(mouseFn, { device: keyboardAddr.device, function: 1 });
    expect(mouseAddr.device).toBe(keyboardAddr.device);
    expect(mouseAddr.function).toBe(1);
  });
});

