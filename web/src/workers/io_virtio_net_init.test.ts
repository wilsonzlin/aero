import { describe, expect, it } from "vitest";

import { DeviceManager, type IrqSink } from "../io/device_manager";
import type { WasmApi } from "../runtime/wasm_context";
import { tryInitVirtioNetDevice } from "./io_virtio_net_init";

describe("workers/io_virtio_net_init", () => {
  it("does not throw when the VirtioNetPciBridge export is missing", () => {
    const irqSink: IrqSink = { raiseIrq: () => {}, lowerIrq: () => {} };
    const mgr = new DeviceManager(irqSink);

    expect(() => {
      const dev = tryInitVirtioNetDevice({
        api: {} as unknown as WasmApi,
        mgr,
        guestBase: 0x1000,
        guestSize: 0x2000,
        ioIpc: new SharedArrayBuffer(1024),
      });
      expect(dev).toBeNull();
    }).not.toThrow();
  });

  it("prefers the 3-arg VirtioNetPciBridge(guestBase, guestSize, ioIpcSab) constructor in modern mode", () => {
    const irqSink: IrqSink = { raiseIrq: () => {}, lowerIrq: () => {} };
    const mgr = new DeviceManager(irqSink);

    let observedArgsLen: number | null = null;
    class FakeVirtioNetPciBridge {
      constructor(_guestBase: number, _guestSize: number, _ioIpc: SharedArrayBuffer) {
        observedArgsLen = arguments.length;
      }

      mmio_read(_offset: number, _size: number): number {
        return 0;
      }
      mmio_write(_offset: number, _size: number, _value: number): void {}
      poll(): void {}
      irq_asserted(): boolean {
        return false;
      }
      free(): void {}
    }

    const dev = tryInitVirtioNetDevice({
      api: { VirtioNetPciBridge: FakeVirtioNetPciBridge } as unknown as WasmApi,
      mgr,
      guestBase: 0x1000,
      guestSize: 0x2000,
      ioIpc: new SharedArrayBuffer(1024),
    });
    expect(dev).not.toBeNull();
    expect(observedArgsLen).toBe(3);
    dev?.destroy();
  });

  it("falls back to the 4-arg VirtioNetPciBridge constructor when the 3-arg form throws (modern mode)", () => {
    const irqSink: IrqSink = { raiseIrq: () => {}, lowerIrq: () => {} };
    const mgr = new DeviceManager(irqSink);

    const observed: number[] = [];
    class FakeVirtioNetPciBridge {
      constructor(_guestBase: number, _guestSize: number, _ioIpc: SharedArrayBuffer, _mode?: unknown) {
        observed.push(arguments.length);
        if (arguments.length === 3) {
          // Simulate a wasm-bindgen build that enforces 4-arg arity (e.g. transport selector required).
          throw new Error("expected 4 args");
        }
      }

      mmio_read(_offset: number, _size: number): number {
        return 0;
      }
      mmio_write(_offset: number, _size: number, _value: number): void {}
      poll(): void {}
      irq_asserted(): boolean {
        return false;
      }
      free(): void {}
    }

    const dev = tryInitVirtioNetDevice({
      api: { VirtioNetPciBridge: FakeVirtioNetPciBridge } as unknown as WasmApi,
      mgr,
      guestBase: 0x1000,
      guestSize: 0x2000,
      ioIpc: new SharedArrayBuffer(1024),
    });
    expect(dev).not.toBeNull();
    expect(observed).toEqual([3, 4]);
    dev?.destroy();
  });

  it("registers a virtio-net PCI device with the Aero Win7 contract v1 identity + config", () => {
    const irqSink: IrqSink = { raiseIrq: () => {}, lowerIrq: () => {} };
    const mgr = new DeviceManager(irqSink);

    const cfgAddrForDev = (dev: number, off: number): number => (0x8000_0000 | ((dev & 0x1f) << 11) | (off & 0xfc)) >>> 0;
    const readCfg8ForDev = (dev: number, off: number): number => {
      mgr.portWrite(0x0cf8, 4, cfgAddrForDev(dev, off));
      return mgr.portRead(0x0cfc + (off & 3), 1) & 0xff;
    };
    const readCfg32ForDev = (dev: number, off: number): number => {
      mgr.portWrite(0x0cf8, 4, cfgAddrForDev(dev, off));
      return mgr.portRead(0x0cfc, 4) >>> 0;
    };
    const writeCfg32ForDev = (dev: number, off: number, value: number): void => {
      mgr.portWrite(0x0cf8, 4, cfgAddrForDev(dev, off));
      mgr.portWrite(0x0cfc, 4, value >>> 0);
    };

    class FakeVirtioNetPciBridge {
      constructor(_guestBase: number, _guestSize: number, _ioIpc: SharedArrayBuffer) {}

      mmio_read(_offset: number, _size: number): number {
        return 0;
      }
      mmio_write(_offset: number, _size: number, _value: number): void {}
      poll(): void {}
      irq_asserted(): boolean {
        return false;
      }
      free(): void {}
    }

    const dev = tryInitVirtioNetDevice({
      api: { VirtioNetPciBridge: FakeVirtioNetPciBridge } as unknown as WasmApi,
      mgr,
      guestBase: 0x1000,
      guestSize: 0x2000,
      ioIpc: new SharedArrayBuffer(1024),
    });
    expect(dev).not.toBeNull();

    let pciDevNum: number | null = null;
    for (let candidate = 0; candidate < 32; candidate++) {
      const id = readCfg32ForDev(candidate, 0x00);
      const vendor = id & 0xffff;
      const device = (id >>> 16) & 0xffff;
      if (vendor === 0x1af4 && device === 0x1041) {
        pciDevNum = candidate;
        break;
      }
    }
    expect(pciDevNum).not.toBeNull();
    const DEV = pciDevNum!;

    const readCfg8 = (off: number): number => readCfg8ForDev(DEV, off);
    const readCfg32 = (off: number): number => readCfg32ForDev(DEV, off);
    const writeCfg32 = (off: number, value: number): void => writeCfg32ForDev(DEV, off, value);

    // Vendor ID (low 16) / Device ID (high 16).
    const id = readCfg32(0x00);
    expect(id & 0xffff).toBe(0x1af4);
    expect((id >>> 16) & 0xffff).toBe(0x1041);

    // Contract v1 uses PCI Revision ID 0x01.
    expect(readCfg8(0x08)).toBe(0x01);

    // Probe BAR0 size mask via the standard all-ones write.
    writeCfg32(0x10, 0xffff_ffff);
    const maskLo = readCfg32(0x10);
    // 64-bit memory BAR sizing mask includes the type bits (0b10 << 1 => 0x4).
    expect(maskLo).toBe(0xffff_c004);

    // Probe BAR1 (high dword) for a 64-bit BAR.
    writeCfg32(0x14, 0xffff_ffff);
    const maskHi = readCfg32(0x14);
    expect(maskHi).toBe(0xffff_ffff);

    // Ensure a PCI capability list is installed (virtio-pci modern transport requires this).
    const cmdStatus = readCfg32(0x04);
    const status = (cmdStatus >>> 16) & 0xffff;
    expect(status & 0x0010).not.toBe(0);

    // Validate virtio-pci vendor-specific capabilities and fixed BAR0 layout (contract v1).
    const cap0 = readCfg8(0x34);
    expect(cap0).not.toBe(0);
    expect((cap0 & 3) >>> 0).toBe(0);

    type VirtioPciCap = { bar: number; offset: number; length: number; notifyOffMultiplier?: number; capLen: number };
    const caps = new Map<number, VirtioPciCap>();
    let cap = cap0;
    for (let i = 0; cap !== 0 && i < 32; i++) {
      const capId = readCfg8(cap);
      const next = readCfg8(cap + 1);
      if (capId === 0x09) {
        const capLen = readCfg8(cap + 2);
        const cfgType = readCfg8(cap + 3);
        const bar = readCfg8(cap + 4);
        const offset = readCfg32(cap + 8);
        const length = readCfg32(cap + 12);
        const info: VirtioPciCap = { bar, offset, length, capLen };
        if (cfgType === 2 && capLen >= 20) {
          info.notifyOffMultiplier = readCfg32(cap + 16);
        }
        caps.set(cfgType, info);
      }
      cap = next;
    }

    expect(caps.get(1)).toEqual({ bar: 0, offset: 0x0000, length: 0x0100, capLen: 16 });
    expect(caps.get(2)).toEqual({ bar: 0, offset: 0x1000, length: 0x0100, capLen: 20, notifyOffMultiplier: 4 });
    expect(caps.get(3)).toEqual({ bar: 0, offset: 0x2000, length: 0x0020, capLen: 16 });
    expect(caps.get(4)).toEqual({ bar: 0, offset: 0x3000, length: 0x0100, capLen: 16 });

    dev?.destroy();
  });

  it("registers a virtio-net PCI device (transitional, BAR2 io size 0x100) and wires legacy io handlers", () => {
    const irqSink: IrqSink = { raiseIrq: () => {}, lowerIrq: () => {} };
    const mgr = new DeviceManager(irqSink);

    // Canonical virtio-net BDF: 00:08.0.
    const DEV = 8;
    const cfgAddr = (off: number): number => (0x8000_0000 | (DEV << 11) | (off & 0xfc)) >>> 0;
    const readCfg32 = (off: number): number => {
      mgr.portWrite(0x0cf8, 4, cfgAddr(off));
      return mgr.portRead(0x0cfc, 4) >>> 0;
    };
    const writeCfg32 = (off: number, value: number): void => {
      mgr.portWrite(0x0cf8, 4, cfgAddr(off));
      mgr.portWrite(0x0cfc, 4, value >>> 0);
    };

    let lastLegacyWrite: { offset: number; size: number; value: number } | null = null;

    class FakeVirtioNetPciBridge {
      constructor(_guestBase: number, _guestSize: number, _ioIpc: SharedArrayBuffer) {}

      mmio_read(_offset: number, _size: number): number {
        return 0;
      }
      mmio_write(_offset: number, _size: number, _value: number): void {}

      io_read(offset: number, size: number): number {
        // Return a stable, non-default value for HOST_FEATURES (offset 0).
        if (offset === 0 && size === 4) return 0x1234_5678;
        return 0;
      }
      io_write(offset: number, size: number, value: number): void {
        lastLegacyWrite = { offset, size, value };
      }

      poll(): void {}
      irq_asserted(): boolean {
        return false;
      }
      free(): void {}
    }

    const dev = tryInitVirtioNetDevice({
      api: { VirtioNetPciBridge: FakeVirtioNetPciBridge } as unknown as WasmApi,
      mgr,
      guestBase: 0x1000,
      guestSize: 0x2000,
      ioIpc: new SharedArrayBuffer(1024),
      mode: "transitional",
    });
    expect(dev).not.toBeNull();

    // Vendor ID (low 16) / Device ID (high 16).
    const id = readCfg32(0x00);
    expect(id & 0xffff).toBe(0x1af4);
    expect((id >>> 16) & 0xffff).toBe(0x1000);

    // BAR2 should be present and decode as I/O (bit0=1).
    const bar2 = readCfg32(0x18);
    expect(bar2 & 0x1).toBe(0x1);
    const ioBase = bar2 & 0xffff_fffc;
    expect(ioBase).toBeGreaterThan(0);

    // Enable PCI I/O decoding (command bit0) so the BAR2 ports are mapped.
    writeCfg32(0x04, 0x0000_0003);

    // Verify legacy HOST_FEATURES is forwarded to the bridge.
    const hostFeatures = mgr.portRead(ioBase + 0x00, 4) >>> 0;
    expect(hostFeatures).toBe(0x1234_5678);

    // Writes should be forwarded without throwing.
    mgr.portWrite(ioBase + 0x04, 4, 0x9abc_def0);
    expect(lastLegacyWrite).toEqual({ offset: 0x04, size: 4, value: 0x9abc_def0 });

    dev?.destroy();
  });
});
