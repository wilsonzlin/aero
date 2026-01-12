import { describe, expect, it } from "vitest";

import { MmioBus } from "../bus/mmio.ts";
import { PciBus } from "../bus/pci.ts";
import { PortIoBus } from "../bus/portio.ts";
import { VirtioInputPciFunction } from "./virtio_input.ts";
import type { VirtioInputPciDeviceLike } from "./virtio_input.ts";

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

function readCapFieldU32(cfg: ReturnType<typeof makeCfgIo>, dev: number, fn: number, capOff: number, off: number): number {
  return cfg.readU32(dev, fn, capOff + off) >>> 0;
}

function probeMmio64BarSize(cfg: ReturnType<typeof makeCfgIo>, dev: number, fn: number, barOff: number): bigint {
  cfg.writeU32(dev, fn, barOff, 0xffff_ffff);
  cfg.writeU32(dev, fn, barOff + 4, 0xffff_ffff);
  const maskLow = cfg.readU32(dev, fn, barOff) >>> 0;
  const maskHigh = cfg.readU32(dev, fn, barOff + 4) >>> 0;
  // Avoid JS bitwise ops on the low dword: values like 0xffff_c000 exceed 2^31 and would
  // sign-extend if we did `maskLow & 0xffff_fff0` before converting to BigInt.
  const mask = (BigInt(maskHigh) << 32n) | (BigInt(maskLow) & 0xffff_fff0n);
  return (~mask + 1n) & 0xffff_ffff_ffff_ffffn;
}

describe("io/devices/virtio_input PCI config", () => {
  it("exposes canonical virtio vendor-specific capabilities at 0x40/0x50/0x64/0x74", () => {
    const portBus = new PortIoBus();
    const mmioBus = new MmioBus();
    const pciBus = new PciBus(portBus, mmioBus);
    pciBus.registerToPortBus();

    const dev: VirtioInputPciDeviceLike = {
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

    const irqSink = { raiseIrq: () => {}, lowerIrq: () => {} };

    const fn0 = new VirtioInputPciFunction({ kind: "keyboard", device: dev, irqSink });
    const fn1 = new VirtioInputPciFunction({ kind: "mouse", device: dev, irqSink });

    expect(fn0.bdf).toEqual({ bus: 0, device: 10, function: 0 });
    expect(fn1.bdf).toEqual({ bus: 0, device: 10, function: 1 });

    // Register at the canonical BDFs via the device-provided defaults.
    const addr0 = pciBus.registerDevice(fn0);
    const addr1 = pciBus.registerDevice(fn1);
    expect(addr0).toEqual(fn0.bdf);
    expect(addr1).toEqual(fn1.bdf);

    const cfg = makeCfgIo(portBus);

    // Vendor/device IDs: 1AF4:1052
    expect(cfg.readU32(10, 0, 0x00)).toBe(0x1052_1af4);
    expect(cfg.readU32(10, 1, 0x00)).toBe(0x1052_1af4);

    // Subsystem vendor: 1AF4; subsystem IDs: 0x0010 (kbd) / 0x0011 (mouse)
    expect(cfg.readU32(10, 0, 0x2c)).toBe(0x0010_1af4);
    expect(cfg.readU32(10, 1, 0x2c)).toBe(0x0011_1af4);

    // Revision ID.
    expect(cfg.readU8(10, 0, 0x08)).toBe(0x01);
    expect(cfg.readU8(10, 1, 0x08)).toBe(0x01);

    // Class code: 0x09_80_00 (Input device, Other).
    expect(cfg.readU8(10, 0, 0x09)).toBe(0x00); // prog-if
    expect(cfg.readU8(10, 0, 0x0a)).toBe(0x80); // subclass
    expect(cfg.readU8(10, 0, 0x0b)).toBe(0x09); // base class

    // Header type: fn0 advertises multifunction.
    expect(cfg.readU8(10, 0, 0x0e)).toBe(0x80);
    expect(cfg.readU8(10, 1, 0x0e)).toBe(0x00);

    // Interrupt line/pin: IRQ 5, INTA#.
    expect(cfg.readU8(10, 0, 0x3c)).toBe(0x05);
    expect(cfg.readU8(10, 0, 0x3d)).toBe(0x01);

    // BAR0: 64-bit MMIO with size 0x4000.
    for (const fn of [0, 1]) {
      const bar0Low = cfg.readU32(10, fn, 0x10);
      const bar0High = cfg.readU32(10, fn, 0x14);
      expect(bar0Low & 0x0f).toBe(0x04);
      expect(bar0High).toBe(0x0000_0000);

      const size = probeMmio64BarSize(cfg, 10, fn, 0x10);
      expect(size).toBe(0x4000n);
    }

    for (const fn of [0, 1]) {
      // Capability list present.
      const status = cfg.readU16(10, fn, 0x06);
      expect(status & 0x0010).toBe(0x0010);
      expect(cfg.readU8(10, fn, 0x34)).toBe(0x40);

      // Cap chain: 0x40 -> 0x50 -> 0x64 -> 0x74 -> 0x00
      expect(cfg.readU8(10, fn, 0x40)).toBe(0x09);
      expect(cfg.readU8(10, fn, 0x41)).toBe(0x50);
      expect(cfg.readU8(10, fn, 0x50)).toBe(0x09);
      expect(cfg.readU8(10, fn, 0x51)).toBe(0x64);
      expect(cfg.readU8(10, fn, 0x64)).toBe(0x09);
      expect(cfg.readU8(10, fn, 0x65)).toBe(0x74);
      expect(cfg.readU8(10, fn, 0x74)).toBe(0x09);
      expect(cfg.readU8(10, fn, 0x75)).toBe(0x00);

      // COMMON_CFG @ 0x40 (cap_len=16)
      expect(cfg.readU8(10, fn, 0x42)).toBe(16);
      expect(cfg.readU8(10, fn, 0x43)).toBe(1); // cfg_type
      expect(cfg.readU8(10, fn, 0x44)).toBe(0); // bar
      expect(readCapFieldU32(cfg, 10, fn, 0x40, 8)).toBe(0x0000);
      expect(readCapFieldU32(cfg, 10, fn, 0x40, 12)).toBe(0x0100);

      // NOTIFY_CFG @ 0x50 (cap_len=20, notify_off_multiplier=4)
      expect(cfg.readU8(10, fn, 0x52)).toBe(20);
      expect(cfg.readU8(10, fn, 0x53)).toBe(2);
      expect(cfg.readU8(10, fn, 0x54)).toBe(0);
      expect(readCapFieldU32(cfg, 10, fn, 0x50, 8)).toBe(0x1000);
      expect(readCapFieldU32(cfg, 10, fn, 0x50, 12)).toBe(0x0100);
      expect(readCapFieldU32(cfg, 10, fn, 0x50, 16)).toBe(4);

      // ISR_CFG @ 0x64 (cap_len=16)
      expect(cfg.readU8(10, fn, 0x66)).toBe(16);
      expect(cfg.readU8(10, fn, 0x67)).toBe(3);
      expect(cfg.readU8(10, fn, 0x68)).toBe(0);
      expect(readCapFieldU32(cfg, 10, fn, 0x64, 8)).toBe(0x2000);
      expect(readCapFieldU32(cfg, 10, fn, 0x64, 12)).toBe(0x0020);

      // DEVICE_CFG @ 0x74 (cap_len=16)
      expect(cfg.readU8(10, fn, 0x76)).toBe(16);
      expect(cfg.readU8(10, fn, 0x77)).toBe(4);
      expect(cfg.readU8(10, fn, 0x78)).toBe(0);
      expect(readCapFieldU32(cfg, 10, fn, 0x74, 8)).toBe(0x3000);
      expect(readCapFieldU32(cfg, 10, fn, 0x74, 12)).toBe(0x0100);
    }
  });
});
