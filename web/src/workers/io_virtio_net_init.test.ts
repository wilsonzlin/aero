import { describe, expect, it } from "vitest";

import { DeviceManager, type IrqSink } from "../io/device_manager";
import { tryInitVirtioNetDevice } from "./io_virtio_net_init";

describe("workers/io_virtio_net_init", () => {
  it("does not throw when the VirtioNetPciBridge export is missing", () => {
    const irqSink: IrqSink = { raiseIrq: () => {}, lowerIrq: () => {} };
    const mgr = new DeviceManager(irqSink);

    expect(() => {
      const dev = tryInitVirtioNetDevice({
        // eslint-disable-next-line @typescript-eslint/no-explicit-any
        api: {} as any,
        mgr,
        guestBase: 0x1000,
        guestSize: 0x2000,
        ioIpc: new SharedArrayBuffer(1024),
      });
      expect(dev).toBeNull();
    }).not.toThrow();
  });

  it("registers a virtio-net PCI device with the Aero Win7 contract v1 identity + config", () => {
    const irqSink: IrqSink = { raiseIrq: () => {}, lowerIrq: () => {} };
    const mgr = new DeviceManager(irqSink);

    // Canonical virtio-net BDF: 00:08.0.
    const DEV = 8;
    const cfgAddr = (off: number): number => (0x8000_0000 | (DEV << 11) | (off & 0xfc)) >>> 0;
    const readCfg8 = (off: number): number => {
      mgr.portWrite(0x0cf8, 4, cfgAddr(off));
      return mgr.portRead(0x0cfc + (off & 3), 1) & 0xff;
    };
    const readCfg32 = (off: number): number => {
      mgr.portWrite(0x0cf8, 4, cfgAddr(off));
      return mgr.portRead(0x0cfc, 4) >>> 0;
    };
    const writeCfg32 = (off: number, value: number): void => {
      mgr.portWrite(0x0cf8, 4, cfgAddr(off));
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
      api: { VirtioNetPciBridge: FakeVirtioNetPciBridge } as any,
      mgr,
      guestBase: 0x1000,
      guestSize: 0x2000,
      ioIpc: new SharedArrayBuffer(1024),
    });
    expect(dev).not.toBeNull();

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
    expect(cap0).toBe(0x50);

    // COMMON cfg at 0x50.
    expect(readCfg8(0x50)).toBe(0x09); // vendor-specific cap
    expect(readCfg8(0x51)).toBe(0x60); // next
    expect(readCfg8(0x52)).toBe(16); // cap_len
    expect(readCfg8(0x53)).toBe(1); // cfg_type
    expect(readCfg8(0x54)).toBe(0); // bar
    expect(readCfg32(0x58)).toBe(0x0000);
    expect(readCfg32(0x5c)).toBe(0x0100);

    // NOTIFY cfg at 0x60.
    expect(readCfg8(0x60)).toBe(0x09);
    expect(readCfg8(0x61)).toBe(0x74);
    expect(readCfg8(0x62)).toBe(20);
    expect(readCfg8(0x63)).toBe(2);
    expect(readCfg32(0x68)).toBe(0x1000);
    expect(readCfg32(0x6c)).toBe(0x0100);
    expect(readCfg32(0x70)).toBe(4);

    // ISR cfg at 0x74.
    expect(readCfg8(0x74)).toBe(0x09);
    expect(readCfg8(0x75)).toBe(0x84);
    expect(readCfg8(0x76)).toBe(16);
    expect(readCfg8(0x77)).toBe(3);
    expect(readCfg32(0x7c)).toBe(0x2000);
    expect(readCfg32(0x80)).toBe(0x0020);

    // DEVICE cfg at 0x84.
    expect(readCfg8(0x84)).toBe(0x09);
    expect(readCfg8(0x85)).toBe(0x00);
    expect(readCfg8(0x86)).toBe(16);
    expect(readCfg8(0x87)).toBe(4);
    expect(readCfg32(0x8c)).toBe(0x3000);
    expect(readCfg32(0x90)).toBe(0x0100);

    dev?.destroy();
  });
});
