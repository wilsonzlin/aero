import { describe, expect, it } from "vitest";

import { PCI_MMIO_BASE } from "../../arch/guest_phys.ts";
import { computeGuestRamLayout } from "../../runtime/shared_layout.ts";
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
    writeU16(dev: number, fn: number, off: number, value: number): void {
      portBus.write(0x0cf8, 4, cfgAddr(dev, fn, off));
      portBus.write(0x0cfc + (off & 3), 2, value & 0xffff);
    },
    writeU8(dev: number, fn: number, off: number, value: number): void {
      portBus.write(0x0cf8, 4, cfgAddr(dev, fn, off));
      portBus.write(0x0cfc + (off & 3), 1, value & 0xff);
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

  it("uses PciDevice.bdf as the default registration address", () => {
    const portBus = new PortIoBus();
    const mmioBus = new MmioBus();
    const pciBus = new PciBus(portBus, mmioBus);
    pciBus.registerToPortBus();

    const dev: PciDevice = {
      name: "bdf_dev",
      vendorId: 0x1234,
      deviceId: 0x5678,
      classCode: 0,
      bdf: { bus: 0, device: 7, function: 0 },
    };
    const addr = pciBus.registerDevice(dev);
    expect(addr).toEqual({ bus: 0, device: 7, function: 0 });

    const cfg = makeCfgIo(portBus);
    expect(cfg.readU32(7, 0, 0x00)).toBe(0x5678_1234);
  });

  it("allows overriding PciDevice.bdf via an explicit registerDevice() address", () => {
    const portBus = new PortIoBus();
    const mmioBus = new MmioBus();
    const pciBus = new PciBus(portBus, mmioBus);
    pciBus.registerToPortBus();

    const dev: PciDevice = {
      name: "bdf_override_dev",
      vendorId: 0x1234,
      deviceId: 0x5678,
      classCode: 0,
      bdf: { bus: 0, device: 7, function: 0 },
    };
    const addr = pciBus.registerDevice(dev, { device: 3, function: 0 });
    expect(addr).toEqual({ bus: 0, device: 3, function: 0 });

    const cfg = makeCfgIo(portBus);
    expect(cfg.readU32(3, 0, 0x00)).toBe(0x5678_1234);
    // The original bdf should remain empty.
    expect(cfg.readU32(7, 0, 0x00)).toBe(0xffff_ffff);
  });

  it("throws when registering two devices that request the same BDF", () => {
    const portBus = new PortIoBus();
    const mmioBus = new MmioBus();
    const pciBus = new PciBus(portBus, mmioBus);
    pciBus.registerToPortBus();

    const a: PciDevice = {
      name: "a",
      vendorId: 0x1111,
      deviceId: 0x2222,
      classCode: 0,
      bdf: { bus: 0, device: 3, function: 0 },
    };
    const b: PciDevice = {
      name: "b",
      vendorId: 0x3333,
      deviceId: 0x4444,
      classCode: 0,
      bdf: { bus: 0, device: 3, function: 0 },
    };
    pciBus.registerDevice(a);
    expect(() => pciBus.registerDevice(b)).toThrow();
  });

  it("rejects unsupported bus numbers in PciDevice.bdf", () => {
    const portBus = new PortIoBus();
    const mmioBus = new MmioBus();
    const pciBus = new PciBus(portBus, mmioBus);
    pciBus.registerToPortBus();

    const dev: PciDevice = {
      name: "bad_bus",
      vendorId: 0x1111,
      deviceId: 0x2222,
      classCode: 0,
      bdf: { bus: 1, device: 0, function: 0 },
    };
    expect(() => pciBus.registerDevice(dev)).toThrow(/only PCI bus 0 is supported/i);
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
    expect(cfg.readU16(addr.device, addr.function, 0x2c)).toBe(0x1234);
    expect(cfg.readU16(addr.device, addr.function, 0x2e)).toBe(0x5678);
  });

  it("allows overriding Subsystem Vendor ID / Subsystem ID (0x2c..0x2f)", () => {
    const portBus = new PortIoBus();
    const mmioBus = new MmioBus();
    const pciBus = new PciBus(portBus, mmioBus);
    pciBus.registerToPortBus();

    const dev: PciDevice = {
      name: "subsys_override",
      vendorId: 0x1234,
      deviceId: 0x5678,
      classCode: 0,
      subsystemVendorId: 0xaaaa,
      subsystemId: 0xbbbb,
    };
    const addr = pciBus.registerDevice(dev);

    const cfg = makeCfgIo(portBus);
    expect(cfg.readU32(addr.device, addr.function, 0x2c)).toBe(0xbbbb_aaaa);
  });

  it("writes Interrupt Pin (0x3d) and defaults to INTA#", () => {
    const portBus = new PortIoBus();
    const mmioBus = new MmioBus();
    const pciBus = new PciBus(portBus, mmioBus);
    pciBus.registerToPortBus();

    const devDefault: PciDevice = { name: "int_default", vendorId: 0x1111, deviceId: 0x2222, classCode: 0 };
    const devIntd: PciDevice = { name: "int_intd", vendorId: 0x3333, deviceId: 0x4444, classCode: 0, interruptPin: 4 };
    const addr0 = pciBus.registerDevice(devDefault, { device: 0, function: 0 });
    const addr1 = pciBus.registerDevice(devIntd, { device: 1, function: 0 });

    const cfg = makeCfgIo(portBus);
    expect(cfg.readU8(addr0.device, addr0.function, 0x3d)).toBe(0x01);
    expect(cfg.readU8(addr1.device, addr1.function, 0x3d)).toBe(0x04);
  });

  it("treats SSVID/SSID + Interrupt Pin as RO while keeping Interrupt Line writable", () => {
    const portBus = new PortIoBus();
    const mmioBus = new MmioBus();
    const pciBus = new PciBus(portBus, mmioBus);
    pciBus.registerToPortBus();

    const dev: PciDevice = {
      name: "rw_ro_dev",
      vendorId: 0x1234,
      deviceId: 0x5678,
      classCode: 0,
      subsystemVendorId: 0xabcd,
      subsystemId: 0xef01,
      irqLine: 0x0b,
      interruptPin: 2,
    };
    const addr = pciBus.registerDevice(dev, { device: 0, function: 0 });

    const cfg = makeCfgIo(portBus);
    expect(cfg.readU32(addr.device, addr.function, 0x2c)).toBe(0xef01_abcd);

    // Guest writes to Subsystem IDs should be ignored (RO).
    cfg.writeU32(addr.device, addr.function, 0x2c, 0);
    expect(cfg.readU32(addr.device, addr.function, 0x2c)).toBe(0xef01_abcd);

    // Interrupt line should be writable, but interrupt pin should be RO.
    expect(cfg.readU8(addr.device, addr.function, 0x3c)).toBe(0x0b);
    expect(cfg.readU8(addr.device, addr.function, 0x3d)).toBe(0x02);

    // Attempt to write both bytes at once.
    cfg.writeU16(addr.device, addr.function, 0x3c, 0x040c); // line=0x0c, pin=0x04
    expect(cfg.readU8(addr.device, addr.function, 0x3c)).toBe(0x0c);
    expect(cfg.readU8(addr.device, addr.function, 0x3d)).toBe(0x02);

    // Attempt to write just the pin byte.
    cfg.writeU8(addr.device, addr.function, 0x3d, 0x03);
    expect(cfg.readU8(addr.device, addr.function, 0x3d)).toBe(0x02);
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

  it("notifies devices of PCI command register writes", () => {
    const portBus = new PortIoBus();
    const mmioBus = new MmioBus();
    const pciBus = new PciBus(portBus, mmioBus);
    pciBus.registerToPortBus();

    let seen: number | null = null;
    const dev: PciDevice = {
      name: "cmd_hook",
      vendorId: 0x1234,
      deviceId: 0x5678,
      classCode: 0,
      onPciCommandWrite: (command) => {
        seen = command;
      },
    };
    const addr = pciBus.registerDevice(dev, { device: 0, function: 0 });

    const cfg = makeCfgIo(portBus);
    cfg.writeU16(addr.device, addr.function, 0x04, 0x0007);
    expect(seen).toBe(0x0007);
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
    // Verify BAR0 encodes a 64-bit memory BAR (bits 3:0 = 0b0100) and the upper
    // 32 bits are present in BAR1.
    const bar0Low = cfg.readU32(0, 0, 0x10);
    const bar0High = cfg.readU32(0, 0, 0x14);
    expect(bar0Low & 0x0f).toBe(0x04);
    expect(bar0High).toBe(0x0000_0000);

    // Write all-ones to both halves of BAR0.
    cfg.writeU32(0, 0, 0x10, 0xffff_ffff);
    cfg.writeU32(0, 0, 0x14, 0xffff_ffff);

    // For size=0x4000, mask=0xFFFF_FFFF_FFFF_C000.
    // Low dword must include type bits for 64-bit BAR (0x4).
    expect(cfg.readU32(0, 0, 0x10)).toBe(0xffff_c004);
    expect(cfg.readU32(0, 0, 0x14)).toBe(0xffff_ffff);
  });

  it("rejects mmio32 BAR allocations above 4GiB (unrepresentable in config space)", () => {
    const portBus = new PortIoBus();
    const mmioBus = new MmioBus();
    const pciBus = new PciBus(portBus, mmioBus);
    pciBus.registerToPortBus();

    // Force an allocation that would align up to >= 4GiB starting from the default 0xE000_0000 base.
    const dev: PciDevice = {
      name: "mmio32_overflow_dev",
      vendorId: 0x1234,
      deviceId: 0x5678,
      classCode: 0,
      bars: [{ kind: "mmio32", size: 0x4000_0000 }, null, null, null, null, null],
    };
    expect(() => pciBus.registerDevice(dev, { device: 0, function: 0 })).toThrow(/mmio32 BAR base overflow/i);
  });

  it("supports mmio64 sizing probes for BARs > 4GiB (high dword mask)", () => {
    const portBus = new PortIoBus();
    const mmioBus = new MmioBus();
    const pciBus = new PciBus(portBus, mmioBus);
    pciBus.registerToPortBus();

    // 8GiB: size mask high dword should not be all-ones.
    const dev: PciDevice = {
      name: "mmio64_big_dev",
      vendorId: 0x1234,
      deviceId: 0x5678,
      classCode: 0,
      bars: [{ kind: "mmio64", size: 0x2_0000_0000 }, null, null, null, null, null],
    };
    pciBus.registerDevice(dev, { device: 0, function: 0 });

    const cfg = makeCfgIo(portBus);
    cfg.writeU32(0, 0, 0x10, 0xffff_ffff);
    cfg.writeU32(0, 0, 0x14, 0xffff_ffff);

    // size=0x2_0000_0000 => mask=0xFFFF_FFFE_0000_0000.
    expect(cfg.readU32(0, 0, 0x10)).toBe(0x0000_0004);
    expect(cfg.readU32(0, 0, 0x14)).toBe(0xffff_fffe);
  });

  it("rejects non-power-of-two BAR sizes above 32-bit range (mmio64)", () => {
    const portBus = new PortIoBus();
    const mmioBus = new MmioBus();
    const pciBus = new PciBus(portBus, mmioBus);
    pciBus.registerToPortBus();

    const dev: PciDevice = {
      name: "mmio64_bad_size_dev",
      vendorId: 0x1234,
      deviceId: 0x5678,
      classCode: 0,
      bars: [{ kind: "mmio64", size: 0x1_0000_0001 }, null, null, null, null, null],
    };
    expect(() => pciBus.registerDevice(dev, { device: 0, function: 0 })).toThrow(/power-of-two/i);
  });

  it("rejects invalid mmio64 BAR layouts (BAR5 start / missing reserved slot)", () => {
    const portBus = new PortIoBus();
    const mmioBus = new MmioBus();
    const pciBus = new PciBus(portBus, mmioBus);
    pciBus.registerToPortBus();

    const devBar5: PciDevice = {
      name: "mmio64_bar5",
      vendorId: 0x1234,
      deviceId: 0x5678,
      classCode: 0,
      bars: [null, null, null, null, null, { kind: "mmio64", size: 0x4000 }],
    };
    expect(() => pciBus.registerDevice(devBar5, { device: 0, function: 0 })).toThrow(/mmio64_bar5/i);
    expect(() => pciBus.registerDevice(devBar5, { device: 0, function: 0 })).toThrow(/BAR5/i);

    const devMissingNull: PciDevice = {
      name: "mmio64_missing_null",
      vendorId: 0x1234,
      deviceId: 0x5678,
      classCode: 0,
      bars: [{ kind: "mmio64", size: 0x4000 }, { kind: "io", size: 0x20 }, null, null, null, null],
    };
    expect(() => pciBus.registerDevice(devMissingNull, { device: 1, function: 0 })).toThrow(/mmio64_missing_null/i);
    expect(() => pciBus.registerDevice(devMissingNull, { device: 1, function: 0 })).toThrow(/consumes|must be null/i);
  });

  it("calls initPciConfig() during registration and preserves written fields", () => {
    const portBus = new PortIoBus();
    const mmioBus = new MmioBus();
    const pciBus = new PciBus(portBus, mmioBus);
    pciBus.registerToPortBus();

    const dev: PciDevice = {
      name: "init_cfg_dev",
      vendorId: 0x3333,
      deviceId: 0x4444,
      classCode: 0,
      bars: [{ kind: "mmio32", size: 0x1000 }, null, null, null, null, null],
      initPciConfig: (config) => {
        // Subsystem IDs.
        config[0x2c] = 0x34;
        config[0x2d] = 0x12;
        config[0x2e] = 0x78;
        config[0x2f] = 0x56;

        // Capabilities list present + a dummy capability.
        config[0x06] |= 0x10;
        config[0x34] = 0x50;
        config[0x50] = 0x09; // vendor-specific
        config[0x51] = 0x00; // end of list
        config[0x52] = 0x08; // length (arbitrary)
        config[0x53] = 0x00;
        config[0x54] = 0xde;
        config[0x55] = 0xad;
        config[0x56] = 0xbe;
        config[0x57] = 0xef;
      },
    };

    const addr = pciBus.registerDevice(dev, { device: 0, function: 0 });
    const cfg = makeCfgIo(portBus);

    expect(cfg.readU32(addr.device, addr.function, 0x2c)).toBe(0x5678_1234);
    expect(cfg.readU16(addr.device, addr.function, 0x06) & 0x0010).toBe(0x0010);
    expect(cfg.readU8(addr.device, addr.function, 0x34)).toBe(0x50);
    expect(cfg.readU32(addr.device, addr.function, 0x50)).toBe(0x0008_0009);
    expect(cfg.readU32(addr.device, addr.function, 0x54)).toBe(0xefbe_adde);
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
    // Capability lists live after the standard 0x40-byte type-0 header.
    // The PCI bus installs capabilities starting at 0x40 with 4-byte aligned pointers.
    expect(ptr).toBe(0x40);
    expect(ptr % 4).toBe(0);

    // Verify the installed list layout is stable and 4-byte aligned:
    //  - cap0 (0x09 len=16) @ 0x40
    //  - cap1 (0x09 len=20) @ 0x50
    //  - cap2 (0x05 len=8)  @ 0x64
    expect(cfg.readU8(0, 0, 0x40)).toBe(0x09);
    expect(cfg.readU8(0, 0, 0x41)).toBe(0x50);
    // Vendor-specific caps: bus must set cap_len to the actual length.
    expect(cfg.readU8(0, 0, 0x42)).toBe(16);

    expect(cfg.readU8(0, 0, 0x50)).toBe(0x09);
    expect(cfg.readU8(0, 0, 0x51)).toBe(0x64);
    expect(cfg.readU8(0, 0, 0x52)).toBe(20);

    expect(cfg.readU8(0, 0, 0x64)).toBe(0x05);
    expect(cfg.readU8(0, 0, 0x65)).toBe(0x00);

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

  it("preserves Status.CAP_LIST on 32-bit writes to the Command/Status dword (0x04)", () => {
    const portBus = new PortIoBus();
    const mmioBus = new MmioBus();
    const pciBus = new PciBus(portBus, mmioBus);
    pciBus.registerToPortBus();

    const dev: PciDevice = {
      name: "status_preserve_dev",
      vendorId: 0x1234,
      deviceId: 0x5678,
      classCode: 0,
      bars: [{ kind: "mmio32", size: 0x100 }, null, null, null, null, null],
      initPciConfig: (config) => {
        // PCI Status register bit 4: Capabilities List.
        config[0x06] |= 0x10;
      },
      mmioRead: () => 0x1122_3344,
      mmioWrite: () => {},
    };

    const addr = pciBus.registerDevice(dev, { device: 0, function: 0 });
    const cfg = makeCfgIo(portBus);

    const statusBefore = cfg.readU16(addr.device, addr.function, 0x06);
    expect(statusBefore & 0x0010).toBe(0x0010);

    const bar0 = cfg.readU32(addr.device, addr.function, 0x10);
    const bar0Base = BigInt(bar0) & 0xffff_fff0n;
    expect(mmioBus.read(bar0Base, 4)).toBe(0xffff_ffff);

    // 32-bit write to 0x04 with upper 16 bits = 0 must not clobber Status.
    cfg.writeU32(addr.device, addr.function, 0x04, 0x0000_0002); // Memory Space Enable

    const statusAfter = cfg.readU16(addr.device, addr.function, 0x06);
    expect(cfg.readU16(addr.device, addr.function, 0x04)).toBe(0x0002);
    expect(statusAfter & 0x0010).toBe(0x0010);

    // BAR decoding should be enabled when Command changes.
    expect(mmioBus.read(bar0Base, 4)).toBe(0x1122_3344);
  });

  it("clears PCI Status RW1C bits on 16-bit writes while preserving CAP_LIST", () => {
    const portBus = new PortIoBus();
    const mmioBus = new MmioBus();
    const pciBus = new PciBus(portBus, mmioBus);
    pciBus.registerToPortBus();

    const seen: number[] = [];
    const dev: PciDevice = {
      name: "status_rw1c_u16",
      vendorId: 0x1234,
      deviceId: 0x5678,
      classCode: 0,
      initPciConfig: (config) => {
        // Status.CAP_LIST (bit 4) is RO.
        config[0x06] |= 0x10;
        // Status.Received Master Abort (bit 13) is RW1C.
        config[0x07] |= 0x20;
      },
      onPciCommandWrite: (command) => {
        seen.push(command & 0xffff);
      },
    };
    const addr = pciBus.registerDevice(dev, { device: 0, function: 0 });

    const cfg = makeCfgIo(portBus);
    const statusBefore = cfg.readU16(addr.device, addr.function, 0x06);
    expect(statusBefore & 0x0010).toBe(0x0010);
    expect(statusBefore & 0x2000).toBe(0x2000);

    // Clear Received Master Abort (bit 13) via RW1C write. CAP_LIST must remain set.
    cfg.writeU16(addr.device, addr.function, 0x06, 0x2000);
    const statusAfter = cfg.readU16(addr.device, addr.function, 0x06);
    expect(statusAfter & 0x0010).toBe(0x0010);
    expect(statusAfter & 0x2000).toBe(0x0000);

    // Status-only writes must not be treated as command updates.
    expect(seen).toEqual([]);
  });

  it("does not clear high-byte RW1C Status bits on 8-bit writes to 0x06", () => {
    const portBus = new PortIoBus();
    const mmioBus = new MmioBus();
    const pciBus = new PciBus(portBus, mmioBus);
    pciBus.registerToPortBus();

    const dev: PciDevice = {
      name: "status_rw1c_u8_low",
      vendorId: 0x1234,
      deviceId: 0x5678,
      classCode: 0,
      initPciConfig: (config) => {
        config[0x06] |= 0x10; // CAP_LIST (bit 4)
        config[0x07] |= 0x20; // Received Master Abort (bit 13)
      },
    };
    const addr = pciBus.registerDevice(dev, { device: 0, function: 0 });
    const cfg = makeCfgIo(portBus);

    // Writing to 0x06 only touches the low byte of Status; it must not be treated
    // as a write to 0x07 (where bit 13 lives).
    cfg.writeU8(addr.device, addr.function, 0x06, 0x00);
    const statusAfter = cfg.readU16(addr.device, addr.function, 0x06);
    expect(statusAfter & 0x0010).toBe(0x0010);
    expect(statusAfter & 0x2000).toBe(0x2000);
  });

  it("clears high-byte RW1C Status bits on 8-bit writes to 0x07", () => {
    const portBus = new PortIoBus();
    const mmioBus = new MmioBus();
    const pciBus = new PciBus(portBus, mmioBus);
    pciBus.registerToPortBus();

    const dev: PciDevice = {
      name: "status_rw1c_u8_high",
      vendorId: 0x1234,
      deviceId: 0x5678,
      classCode: 0,
      initPciConfig: (config) => {
        config[0x06] |= 0x10; // CAP_LIST (bit 4)
        config[0x07] |= 0x20; // Received Master Abort (bit 13)
      },
    };
    const addr = pciBus.registerDevice(dev, { device: 0, function: 0 });
    const cfg = makeCfgIo(portBus);

    cfg.writeU8(addr.device, addr.function, 0x07, 0x20);
    const statusAfter = cfg.readU16(addr.device, addr.function, 0x06);
    expect(statusAfter & 0x0010).toBe(0x0010);
    expect(statusAfter & 0x2000).toBe(0x0000);
  });

  it("does not allow initPciConfig() to force-enable decoding or clobber BAR registers", () => {
    const portBus = new PortIoBus();
    const mmioBus = new MmioBus();
    const pciBus = new PciBus(portBus, mmioBus);
    pciBus.registerToPortBus();

    const dev: PciDevice = {
      name: "init_invariant_dev",
      vendorId: 0x1234,
      deviceId: 0x5678,
      classCode: 0,
      bars: [{ kind: "mmio32", size: 0x1000 }, null, null, null, null, null],
      initPciConfig: (config) => {
        // Attempt to enable memory decoding + smash BAR0.
        config[0x04] = 0x02;
        config[0x05] = 0x00;
        config[0x10] = 0xaa;
        config[0x11] = 0xbb;
        config[0x12] = 0xcc;
        config[0x13] = 0xdd;
        // Also attempt to scribble into an unimplemented BAR.
        config[0x14] = 0x11;
        config[0x15] = 0x22;
        config[0x16] = 0x33;
        config[0x17] = 0x44;
      },
      mmioRead: () => 0xdead_beef,
      mmioWrite: () => {},
    };

    const addr = pciBus.registerDevice(dev, { device: 0, function: 0 });
    const cfg = makeCfgIo(portBus);

    // Command should be reset to 0 so decoding is still guest-controlled.
    expect(cfg.readU16(addr.device, addr.function, 0x04)).toBe(0x0000);

    // BAR0 should reflect the bus-assigned base, not the value written by initPciConfig().
    const bar0 = cfg.readU32(addr.device, addr.function, 0x10);
    expect(bar0 >>> 0).toBe(PCI_MMIO_BASE);
    // The web runtime clamps guest RAM to live below the PCI MMIO aperture. Ensure
    // our auto-assigned BAR base is outside the maximum guest RAM region.
    expect(bar0 >>> 0).toBeGreaterThanOrEqual(computeGuestRamLayout(0xffff_ffff).guest_size);

    // BAR1 is unimplemented and must read as 0 even if initPciConfig scribbled.
    expect(cfg.readU32(addr.device, addr.function, 0x14)).toBe(0);

    // MMIO should not be decoded until the guest enables it.
    const base = BigInt(bar0) & 0xffff_fff0n;
    expect(mmioBus.read(base, 4)).toBe(0xffff_ffff);

    cfg.writeU16(addr.device, addr.function, 0x04, 0x0002); // Memory Space Enable
    expect(mmioBus.read(base, 4)).toBe(0xdead_beef);
  });

  it("prevents initPciConfig() from changing the header type (BAR writes must still remap)", () => {
    const portBus = new PortIoBus();
    const mmioBus = new MmioBus();
    const pciBus = new PciBus(portBus, mmioBus);
    pciBus.registerToPortBus();

    const dev: PciDevice = {
      name: "init_header_type_dev",
      vendorId: 0x1234,
      deviceId: 0x5678,
      classCode: 0,
      bars: [{ kind: "mmio32", size: 0x1000 }, null, null, null, null, null],
      initPciConfig: (config) => {
        // Attempt to change the header type away from type-0 (endpoint).
        config[0x0e] = 0x01;
      },
      mmioRead: () => 0x1122_3344,
      mmioWrite: () => {},
    };
    const addr = pciBus.registerDevice(dev, { device: 0, function: 0 });
    const cfg = makeCfgIo(portBus);

    // Header type should remain type 0.
    expect(cfg.readU8(addr.device, addr.function, 0x0e) & 0x7f).toBe(0x00);

    const bar0 = cfg.readU32(addr.device, addr.function, 0x10);
    const base = BigInt(bar0) & 0xffff_fff0n;

    // Enable decoding; BAR0 should map.
    cfg.writeU16(addr.device, addr.function, 0x04, 0x0002);
    expect(mmioBus.read(base, 4)).toBe(0x1122_3344);

    // BAR writes must still remap (requires header type to be correct).
    const newBase = base + 0x2000n;
    cfg.writeU32(addr.device, addr.function, 0x10, Number(newBase & 0xffff_ffffn));
    expect(mmioBus.read(base, 4)).toBe(0xffff_ffff);
    expect(mmioBus.read(newBase, 4)).toBe(0x1122_3344);
  });

  it("remaps mmio64 BARs on low/high writes while respecting Command.MEM enable", () => {
    const portBus = new PortIoBus();
    const mmioBus = new MmioBus();
    const pciBus = new PciBus(portBus, mmioBus);
    pciBus.registerToPortBus();

    const dev: PciDevice = {
      name: "mmio64_remap_dev",
      vendorId: 0x1234,
      deviceId: 0x5678,
      classCode: 0,
      bars: [{ kind: "mmio64", size: 0x4000 }, null, null, null, null, null],
      mmioRead: () => 0x1122_3344,
      mmioWrite: () => {},
    };
    const addr = pciBus.registerDevice(dev, { device: 0, function: 0 });
    const cfg = makeCfgIo(portBus);

    const bar0Low = cfg.readU32(addr.device, addr.function, 0x10);
    const bar0High = cfg.readU32(addr.device, addr.function, 0x14);
    const base = (BigInt(bar0High) << 32n) | (BigInt(bar0Low) & 0xffff_fff0n);

    // Memory decoding is disabled by default.
    expect(mmioBus.read(base, 4)).toBe(0xffff_ffff);

    // Enable memory decoding; BAR0 should map.
    cfg.writeU16(addr.device, addr.function, 0x04, 0x0002);
    expect(mmioBus.read(base, 4)).toBe(0x1122_3344);

    // Move BAR0 to a new base and ensure the old range is unmapped.
    const newBase = base + 0x1_0000n; // keep aligned to 0x4000
    const newLow = Number(newBase & 0xffff_ffffn) >>> 0;
    const newHigh = Number((newBase >> 32n) & 0xffff_ffffn) >>> 0;
    cfg.writeU32(addr.device, addr.function, 0x10, newLow);
    cfg.writeU32(addr.device, addr.function, 0x14, newHigh);

    expect(mmioBus.read(base, 4)).toBe(0xffff_ffff);
    expect(mmioBus.read(newBase, 4)).toBe(0x1122_3344);

    // Disable decoding again.
    cfg.writeU16(addr.device, addr.function, 0x04, 0x0000);
    expect(mmioBus.read(newBase, 4)).toBe(0xffff_ffff);
  });

  it("notifies devices on PCI command register writes (bus master + interrupt disable)", () => {
    const portBus = new PortIoBus();
    const mmioBus = new MmioBus();
    const pciBus = new PciBus(portBus, mmioBus);
    pciBus.registerToPortBus();

    const seen: number[] = [];
    const dev: PciDevice = {
      name: "cmd_dev",
      vendorId: 0x1234,
      deviceId: 0x5678,
      classCode: 0,
      onPciCommandWrite: (command) => {
        seen.push(command & 0xffff);
      },
    };
    pciBus.registerDevice(dev, { device: 0, function: 0 });

    const cfg = makeCfgIo(portBus);
    // Bus Master Enable (bit 2).
    cfg.writeU32(0, 0, 0x04, 0x0000_0004);
    // Interrupt Disable (bit 10) + Bus Master Enable.
    cfg.writeU32(0, 0, 0x04, 0x0000_0404);

    // Writing status bits should not trigger a callback (command unchanged).
    cfg.writeU32(0, 0, 0x04, 0xabcd_0404);

    expect(seen).toEqual([0x0004, 0x0404]);
  });

  it("does not map mmio64 BARs until Command.MEM is enabled, even if BAR base was programmed", () => {
    const portBus = new PortIoBus();
    const mmioBus = new MmioBus();
    const pciBus = new PciBus(portBus, mmioBus);
    pciBus.registerToPortBus();

    const dev: PciDevice = {
      name: "mmio64_mem_enable_dev",
      vendorId: 0x1234,
      deviceId: 0x5678,
      classCode: 0,
      bars: [{ kind: "mmio64", size: 0x4000 }, null, null, null, null, null],
      mmioRead: () => 0x1122_3344,
      mmioWrite: () => {},
    };
    const addr = pciBus.registerDevice(dev, { device: 0, function: 0 });
    const cfg = makeCfgIo(portBus);

    const bar0Low = cfg.readU32(addr.device, addr.function, 0x10);
    const bar0High = cfg.readU32(addr.device, addr.function, 0x14);
    const base = (BigInt(bar0High) << 32n) | (BigInt(bar0Low) & 0xffff_fff0n);

    // Memory decoding is disabled by default, so nothing is mapped.
    expect(mmioBus.read(base, 4)).toBe(0xffff_ffff);

    // Program a new base while MEM is disabled.
    const newBase = base + 0x2_0000n; // keep aligned to 0x4000
    const newLow = Number(newBase & 0xffff_ffffn) >>> 0;
    const newHigh = Number((newBase >> 32n) & 0xffff_ffffn) >>> 0;
    cfg.writeU32(addr.device, addr.function, 0x10, newLow);
    cfg.writeU32(addr.device, addr.function, 0x14, newHigh);

    // Still unmapped until the guest enables MEM.
    expect(mmioBus.read(newBase, 4)).toBe(0xffff_ffff);

    // BAR low dword should keep the 64-bit type bits (0x4).
    expect(cfg.readU32(addr.device, addr.function, 0x10)).toBe(((newLow & 0xffff_fff0) | 0x4) >>> 0);
    expect(cfg.readU32(addr.device, addr.function, 0x14)).toBe(newHigh >>> 0);

    // Enable MEM decoding now; mapping should appear at the programmed base.
    cfg.writeU16(addr.device, addr.function, 0x04, 0x0002);
    expect(mmioBus.read(base, 4)).toBe(0xffff_ffff);
    expect(mmioBus.read(newBase, 4)).toBe(0x1122_3344);
  });

  it("remaps mmio64 BARs when the high dword is updated (mapping above 4GiB)", () => {
    const portBus = new PortIoBus();
    const mmioBus = new MmioBus();
    const pciBus = new PciBus(portBus, mmioBus);
    pciBus.registerToPortBus();

    const dev: PciDevice = {
      name: "mmio64_hi_remap_dev",
      vendorId: 0x1234,
      deviceId: 0x5678,
      classCode: 0,
      bars: [{ kind: "mmio64", size: 0x4000 }, null, null, null, null, null],
      mmioRead: () => 0x1122_3344,
      mmioWrite: () => {},
    };
    const addr = pciBus.registerDevice(dev, { device: 0, function: 0 });
    const cfg = makeCfgIo(portBus);

    // Enable memory decoding.
    cfg.writeU16(addr.device, addr.function, 0x04, 0x0002);

    const bar0Low = cfg.readU32(addr.device, addr.function, 0x10);
    const bar0High = cfg.readU32(addr.device, addr.function, 0x14);
    const base = (BigInt(bar0High) << 32n) | (BigInt(bar0Low) & 0xffff_fff0n);
    expect(mmioBus.read(base, 4)).toBe(0x1122_3344);

    // Move the BAR to above 4GiB by updating only the high dword.
    const newHigh = (bar0High + 1) >>> 0;
    cfg.writeU32(addr.device, addr.function, 0x14, newHigh);
    const newBase = (BigInt(newHigh) << 32n) | (BigInt(bar0Low) & 0xffff_fff0n);

    expect(mmioBus.read(base, 4)).toBe(0xffff_ffff);
    expect(mmioBus.read(newBase, 4)).toBe(0x1122_3344);
  });

  it("snapshots/restores PCI config space for suspend/resume (command + BAR + addrReg)", () => {
    const portBus = new PortIoBus();
    const mmioBus = new MmioBus();
    const pciBus = new PciBus(portBus, mmioBus);
    pciBus.registerToPortBus();

    let reads = 0;
    const dev: PciDevice = {
      name: "snap_dev",
      vendorId: 0x1234,
      deviceId: 0x5678,
      classCode: 0,
      bars: [{ kind: "mmio32", size: 0x1000 }, null, null, null, null, null],
      mmioRead: () => {
        reads++;
        return 0xdead_beef;
      },
    };
    const addr = pciBus.registerDevice(dev, { device: 0, function: 0 });

    const cfg = makeCfgIo(portBus);
    // Enable BAR decoding (MEM enable bit).
    cfg.writeU16(addr.device, addr.function, 0x04, 0x0002);
    // Program BAR0 to a deterministic address.
    cfg.writeU32(addr.device, addr.function, 0x10, 0x8000_0000);

    // Set addrReg to select BAR0.
    portBus.write(0x0cf8, 4, cfgAddr(addr.device, addr.function, 0x10));

    expect(mmioBus.read(0x8000_0000n, 4)).toBe(0xdead_beef);
    expect(reads).toBe(1);

    const snapshot = pciBus.saveState();

    // Restore into a fresh bus with the same device registered.
    const portBus2 = new PortIoBus();
    const mmioBus2 = new MmioBus();
    const pciBus2 = new PciBus(portBus2, mmioBus2);
    pciBus2.registerToPortBus();

    let reads2 = 0;
    const dev2: PciDevice = {
      name: "snap_dev",
      vendorId: 0x1234,
      deviceId: 0x5678,
      classCode: 0,
      bars: [{ kind: "mmio32", size: 0x1000 }, null, null, null, null, null],
      mmioRead: () => {
        reads2++;
        return 0xcafe_babe;
      },
    };
    pciBus2.registerDevice(dev2, { device: 0, function: 0 });

    pciBus2.loadState(snapshot);

    // AddrReg restore: a direct read from 0xCFC should observe BAR0.
    expect(portBus2.read(0x0cfc, 4) >>> 0).toBe(0x8000_0000);

    // BAR mapping + command restore.
    expect(mmioBus2.read(0x8000_0000n, 4)).toBe(0xcafe_babe);
    expect(reads2).toBe(1);
  });
});
