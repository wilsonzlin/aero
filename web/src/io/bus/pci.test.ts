import { describe, expect, it } from "vitest";

import { MmioBus } from "./mmio.ts";
import { PciBus } from "./pci.ts";
import { PortIoBus } from "./portio.ts";
import type { PciCapability, PciDevice } from "./pci.ts";

function cfgAddr(dev: number, fn: number, off: number): number {
  // PCI config mechanism #1 (I/O ports 0xCF8/0xCFC).
  return (0x8000_0000 | ((dev & 0x1f) << 11) | ((fn & 0x07) << 8) | (off & 0xfc)) >>> 0;
}

function makeCfgIo(portBus: PortIoBus) {
  return {
    readU32(dev: number, fn: number, off: number): number {
      portBus.write(0x0cf8, 4, cfgAddr(dev, fn, off));
      return portBus.read(0x0cfc, 4) >>> 0;
    },
    readU16(dev: number, fn: number, off: number): number {
      portBus.write(0x0cf8, 4, cfgAddr(dev, fn, off));
      return portBus.read(0x0cfc + (off & 3), 2) & 0xffff;
    },
    readU8(dev: number, fn: number, off: number): number {
      portBus.write(0x0cf8, 4, cfgAddr(dev, fn, off));
      return portBus.read(0x0cfc + (off & 3), 1) & 0xff;
    },
    writeU32(dev: number, fn: number, off: number, value: number): void {
      portBus.write(0x0cf8, 4, cfgAddr(dev, fn, off));
      portBus.write(0x0cfc, 4, value >>> 0);
    },
  };
}

describe("io/bus/pci", () => {
  it("supports accessing function numbers 0..7 via 0xCF8/0xCFC", () => {
    const portBus = new PortIoBus();
    const mmioBus = new MmioBus();
    const pciBus = new PciBus(portBus, mmioBus);
    pciBus.registerToPortBus();

    const fn0: PciDevice = { name: "fn0", vendorId: 0x1111, deviceId: 0x2222, classCode: 0 };
    const fn1: PciDevice = { name: "fn1", vendorId: 0x3333, deviceId: 0x4444, classCode: 0 };

    pciBus.registerDevice(fn0, { device: 0, function: 0 });
    pciBus.registerDevice(fn1, { device: 0, function: 1 });

    const cfg = makeCfgIo(portBus);
    expect(cfg.readU32(0, 0, 0x00)).toBe(0x2222_1111);
    expect(cfg.readU32(0, 1, 0x00)).toBe(0x4444_3333);
  });

  it("populates Subsystem Vendor ID / Subsystem ID (0x2c..0x2f) by default", () => {
    const portBus = new PortIoBus();
    const mmioBus = new MmioBus();
    const pciBus = new PciBus(portBus, mmioBus);
    pciBus.registerToPortBus();

    const dev: PciDevice = { name: "subsys_dev", vendorId: 0x1234, deviceId: 0x5678, classCode: 0 };
    const addr = pciBus.registerDevice(dev);

    const cfg = makeCfgIo(portBus);

    // Vendor ID (low 16) / Device ID (high 16)
    expect(cfg.readU32(addr.device, addr.function, 0x00)).toBe(0x5678_1234);

    // Subsystem Vendor ID (low 16) / Subsystem ID (high 16)
    expect(cfg.readU32(addr.device, addr.function, 0x2c)).toBe(0x5678_1234);
  });

  it("sets the multifunction bit in header_type (fn0) when additional functions exist", () => {
    const portBus = new PortIoBus();
    const mmioBus = new MmioBus();
    const pciBus = new PciBus(portBus, mmioBus);
    pciBus.registerToPortBus();

    const fn0: PciDevice = { name: "fn0", vendorId: 0x1111, deviceId: 0x2222, classCode: 0 };
    const fn1: PciDevice = { name: "fn1", vendorId: 0x1111, deviceId: 0x2222, classCode: 0 };

    pciBus.registerDevice(fn0, { device: 0, function: 0 });

    const cfg = makeCfgIo(portBus);
    expect(cfg.readU8(0, 0, 0x0e)).toBe(0x00);

    pciBus.registerDevice(fn1, { device: 0, function: 1 });
    expect(cfg.readU8(0, 0, 0x0e)).toBe(0x80);
  });

  it("implements 64-bit MMIO BAR sizing probes (low/high dwords)", () => {
    const portBus = new PortIoBus();
    const mmioBus = new MmioBus();
    const pciBus = new PciBus(portBus, mmioBus);
    pciBus.registerToPortBus();

    const dev: PciDevice = {
      name: "mmio64_dev",
      vendorId: 0x1234,
      deviceId: 0x5678,
      classCode: 0,
      bars: [{ kind: "mmio64", size: 0x4000 }, null, null, null, null, null],
    };
    pciBus.registerDevice(dev, { device: 0, function: 0 });

    const cfg = makeCfgIo(portBus);
    // Write all-ones to both halves of BAR0.
    cfg.writeU32(0, 0, 0x10, 0xffff_ffff);
    cfg.writeU32(0, 0, 0x14, 0xffff_ffff);

    // For size=0x4000, mask=0xFFFF_FFFF_FFFF_C000.
    // Low dword must include type bits for 64-bit BAR (0x4).
    expect(cfg.readU32(0, 0, 0x10)).toBe(0xffff_c004);
    expect(cfg.readU32(0, 0, 0x14)).toBe(0xffff_ffff);
  });

  it("builds a valid, acyclic PCI capability list with aligned pointers", () => {
    const portBus = new PortIoBus();
    const mmioBus = new MmioBus();
    const pciBus = new PciBus(portBus, mmioBus);
    pciBus.registerToPortBus();

    const cap = (id: number, len: number): PciCapability => {
      const bytes = new Uint8Array(len);
      bytes[0] = id & 0xff;
      bytes[1] = 0; // next pointer (patched by bus)
      return { bytes };
    };

    const dev: PciDevice = {
      name: "caps_dev",
      vendorId: 0x1234,
      deviceId: 0x5678,
      classCode: 0,
      capabilities: [cap(0x09, 16), cap(0x09, 20), cap(0x05, 8)],
    };
    pciBus.registerDevice(dev, { device: 0, function: 0 });

    const cfg = makeCfgIo(portBus);
    const status = cfg.readU16(0, 0, 0x06);
    expect(status & 0x0010).toBe(0x0010);

    let ptr = cfg.readU8(0, 0, 0x34);
    expect(ptr).not.toBe(0);
    expect(ptr % 4).toBe(0);

    const seen = new Set<number>();
    while (ptr !== 0) {
      expect(ptr % 4).toBe(0);
      expect(seen.has(ptr)).toBe(false);
      seen.add(ptr);

      const id = cfg.readU8(0, 0, ptr);
      expect(id).toBeGreaterThan(0);
      const next = cfg.readU8(0, 0, ptr + 1);
      ptr = next;
    }

    expect(seen.size).toBe(3);
  });
});

