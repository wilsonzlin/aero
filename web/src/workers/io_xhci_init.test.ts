import { describe, expect, it } from "vitest";

import { DeviceManager } from "../io/device_manager";
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

      tick(_nowMs?: number): void {
        tickCalls++;
      }

      free(): void {}
    }

    const api = { XhciControllerBridge: FakeXhciControllerBridge } as unknown as WasmApi;
    const mgr = new DeviceManager({ raiseIrq: () => {}, lowerIrq: () => {} });

    const res = tryInitXhciDevice({ api, mgr, guestBase: 0x1000_0000, guestSize: 0x0200_0000 });
    expect(res).not.toBeNull();
    const { device: dev, bridge } = res!;
    expect((bridge as unknown as FakeXhciControllerBridge).base).toBe(0x1000_0000);
    expect((bridge as unknown as FakeXhciControllerBridge).size).toBe(0x0200_0000);

    const cfg = makeCfgIo(mgr);
    const bdf = dev.bdf ?? { bus: 0, device: 2, function: 0 };
    expect(cfg.readU32(bdf.device, bdf.function, 0x00)).toBe(((dev.deviceId & 0xffff) << 16) | (dev.vendorId & 0xffff));

    // Enable PCI Bus Master so the JS wrapper calls into the bridge on tick.
    const prevCmd = cfg.readU16(bdf.device, bdf.function, 0x04);
    cfg.writeU16(bdf.device, bdf.function, 0x04, prevCmd | (1 << 2));
    // The xHCI JS wrapper only steps the underlying bridge after observing a positive delta
    // between ticks; the first tick initializes its time base.
    mgr.tick(123);
    mgr.tick(124);
    expect(tickCalls).toBeGreaterThan(0);
  });
});
