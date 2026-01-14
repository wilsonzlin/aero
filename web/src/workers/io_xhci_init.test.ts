import { describe, expect, it } from "vitest";

import { DeviceManager } from "../io/device_manager";
import { XhciPciDevice } from "../io/devices/xhci";
import type { WasmApi } from "../runtime/wasm_context";
import { tryInitXhciDevice } from "./io_xhci_init";

function cfgAddr(dev: number, fn: number, off: number): number {
  return (0x8000_0000 | ((dev & 0x1f) << 11) | ((fn & 0x07) << 8) | (off & 0xfc)) >>> 0;
}

function makeCfgIo(mgr: DeviceManager) {
  return {
    readU32(dev: number, fn: number, off: number): number {
      mgr.portWrite(0x0cf8, 4, cfgAddr(dev, fn, off));
      return mgr.portRead(0x0cfc, 4) >>> 0;
    },
    readU16(dev: number, fn: number, off: number): number {
      mgr.portWrite(0x0cf8, 4, cfgAddr(dev, fn, off));
      return mgr.portRead(0x0cfc + (off & 3), 2) & 0xffff;
    },
    writeU16(dev: number, fn: number, off: number, value: number): void {
      mgr.portWrite(0x0cf8, 4, cfgAddr(dev, fn, off));
      mgr.portWrite(0x0cfc + (off & 3), 2, value & 0xffff);
    },
  };
}

describe("workers/io_xhci_init", () => {
  it("registers an xhci PCI device when XhciControllerBridge is available", () => {
    let tickCalls = 0;

    class FakeXhciControllerBridge {
      readonly base: number;
      readonly size: number | undefined;

      constructor(base: number, size?: number) {
        this.base = base >>> 0;
        this.size = size === undefined ? undefined : (size >>> 0);
      }

      mmio_read(_offset: number, _size: number): number {
        return 0;
      }

      mmio_write(_offset: number, _size: number, _value: number): void {}

      irq_asserted(): boolean {
        return false;
      }

      step_frames(frames: number): void {
        tickCalls += frames;
      }

      step_frame(): void {
        tickCalls++;
      }

      free(): void {}
    }

    const api = { XhciControllerBridge: FakeXhciControllerBridge } as unknown as WasmApi;
    const mgr = new DeviceManager({ raiseIrq: () => {}, lowerIrq: () => {} });

    const res = tryInitXhciDevice({ api, mgr, guestBase: 0x1000_0000, guestSize: 0x0200_0000 });
    expect(res).not.toBeNull();
    const { device: dev, bridge } = res!;
    // xHCI should prefer the canonical 00:0d.0 BDF when unoccupied.
    expect(dev.bdf).toEqual({ bus: 0, device: 0x0d, function: 0 });
    // Canonical QEMU-style xHCI ("qemu-xhci") PCI identity: 1b36:000d.
    expect(dev.vendorId).toBe(0x1b36);
    expect(dev.deviceId).toBe(0x000d);
    expect((bridge as unknown as FakeXhciControllerBridge).base).toBe(0x1000_0000);
    expect((bridge as unknown as FakeXhciControllerBridge).size).toBe(0x0200_0000);

    const cfg = makeCfgIo(mgr);
    expect(cfg.readU32(dev.bdf.device, dev.bdf.function, 0x00)).toBe(((dev.deviceId & 0xffff) << 16) | (dev.vendorId & 0xffff));

    // Enable PCI Bus Master so the JS wrapper calls into the bridge on tick.
    const prevCmd = cfg.readU16(dev.bdf.device, dev.bdf.function, 0x04);
    cfg.writeU16(dev.bdf.device, dev.bdf.function, 0x04, prevCmd | (1 << 2));
    // The xHCI JS wrapper only steps the underlying bridge after observing a positive delta
    // between ticks; the first tick initializes its time base.
    mgr.tick(123);
    mgr.tick(124);
    expect(tickCalls).toBeGreaterThan(0);
  });

  it("supports XhciControllerBridge constructors that enforce zero arguments (wasm-bindgen arity quirk)", () => {
    class FakeXhciControllerBridge {
      readonly argCount: number;

      constructor() {
        this.argCount = arguments.length;
        if (this.argCount !== 0) {
          throw new Error(`expected 0 args, got ${this.argCount}`);
        }
      }

      mmio_read(_offset: number, _size: number): number {
        return 0;
      }
      mmio_write(_offset: number, _size: number, _value: number): void {}
      irq_asserted(): boolean {
        return false;
      }
      free(): void {}
    }

    const api = { XhciControllerBridge: FakeXhciControllerBridge } as unknown as WasmApi;
    const mgr = new DeviceManager({ raiseIrq: () => {}, lowerIrq: () => {} });

    const res = tryInitXhciDevice({ api, mgr, guestBase: 0x1000_0000, guestSize: 0x0200_0000 });
    expect(res).not.toBeNull();
    expect((res!.bridge as unknown as FakeXhciControllerBridge).argCount).toBe(0);
  });

  it("falls back to auto-allocation when the canonical xHCI BDF is already occupied", () => {
    class FakeXhciControllerBridge {
      mmio_read(_offset: number, _size: number): number {
        return 0;
      }
      mmio_write(_offset: number, _size: number, _value: number): void {}
      irq_asserted(): boolean {
        return false;
      }
      free(): void {}
    }

    const api = { XhciControllerBridge: FakeXhciControllerBridge } as unknown as WasmApi;
    const mgr = new DeviceManager({ raiseIrq: () => {}, lowerIrq: () => {} });

    // Occupy the canonical slot requested by `XhciPciDevice`.
    const canonical = new XhciPciDevice({ bridge: new FakeXhciControllerBridge(), irqSink: mgr.irqSink }).bdf;
    mgr.registerPciDevice({
      name: "occupied",
      vendorId: 0x1111,
      deviceId: 0x2222,
      classCode: 0,
      bdf: canonical,
    });

    const res = tryInitXhciDevice({ api, mgr, guestBase: 0x1000_0000, guestSize: 0x0200_0000 });
    expect(res).not.toBeNull();
    const { device: dev } = res!;
    const cfg = makeCfgIo(mgr);

    // The canonical slot should still contain the pre-registered device.
    expect(cfg.readU32(canonical.device, canonical.function, 0x00)).toBe(0x2222_1111);

    // xHCI should have been placed elsewhere.
    expect(dev.bdf).toBeTruthy();
    expect(dev.bdf).not.toEqual(canonical);
    expect(cfg.readU32(dev.bdf!.device, dev.bdf!.function, 0x00)).toBe(((dev.deviceId & 0xffff) << 16) | (dev.vendorId & 0xffff));
  });
});
