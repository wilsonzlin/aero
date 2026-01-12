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

  it("registers a virtio-net PCI device with BAR0 size 0x4000", () => {
    const irqSink: IrqSink = { raiseIrq: () => {}, lowerIrq: () => {} };
    const mgr = new DeviceManager(irqSink);

    const readCfg8 = (off: number): number => {
      mgr.portWrite(0x0cf8, 4, 0x8000_0000 | (off & 0xfc));
      return mgr.portRead(0x0cfc + (off & 3), 1) & 0xff;
    };

    const readCfg32 = (off: number): number => {
      mgr.portWrite(0x0cf8, 4, 0x8000_0000 | (off & 0xfc));
      return mgr.portRead(0x0cfc, 4) >>> 0;
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

    // Read vendor/device IDs for device 8 on bus 0 (canonical virtio-net BDF: 00:08.0).
    mgr.portWrite(0x0cf8, 4, 0x8000_0000 | (8 << 11) | 0x00);
    const id = mgr.portRead(0x0cfc, 4) >>> 0;
    expect(id & 0xffff).toBe(0x1af4);
    expect((id >>> 16) & 0xffff).toBe(0x1041);

    // Contract v1 uses PCI Revision ID 0x01.
    mgr.portWrite(0x0cf8, 4, 0x8000_0000 | 0x08);
    const classRev = mgr.portRead(0x0cfc, 4) >>> 0;
    expect(classRev & 0xff).toBe(0x01);

    // Probe BAR0 size mask via the standard all-ones write.
    mgr.portWrite(0x0cf8, 4, 0x8000_0000 | (8 << 11) | 0x10);
    mgr.portWrite(0x0cfc, 4, 0xffff_ffff);
    const mask = mgr.portRead(0x0cfc, 4) >>> 0;
    // 64-bit memory BAR reports type bits 0b10 in bits 2:1 (0x4).
    expect(mask).toBe(0xffff_c004);

    // Probe BAR1 (high dword) for a 64-bit BAR.
    mgr.portWrite(0x0cf8, 4, 0x8000_0000 | 0x14);
    mgr.portWrite(0x0cfc, 4, 0xffff_ffff);
    const maskHi = mgr.portRead(0x0cfc, 4) >>> 0;
    expect(maskHi).toBe(0xffff_ffff);

    // Ensure a PCI capability list is installed (virtio-pci modern transport requires this).
    mgr.portWrite(0x0cf8, 4, 0x8000_0000 | 0x04);
    const cmdStatus = mgr.portRead(0x0cfc, 4) >>> 0;
    const status = (cmdStatus >>> 16) & 0xffff;
    expect(status & 0x0010).not.toBe(0);

    // Validate virtio-pci vendor-specific capabilities and fixed BAR0 layout (contract v1).
    const cap0 = readCfg8(0x34);
    expect(cap0).toBe(0x40);

    expect(readCfg8(cap0)).toBe(0x09); // vendor-specific cap
    expect(readCfg8(cap0 + 2)).toBe(16); // cap_len
    expect(readCfg8(cap0 + 3)).toBe(1); // common cfg
    expect(readCfg8(cap0 + 4)).toBe(0); // bar
    expect(readCfg32(cap0 + 8)).toBe(0x0000);
    expect(readCfg32(cap0 + 12)).toBe(0x0100);

    const cap1 = readCfg8(cap0 + 1);
    expect(cap1).toBe(0x50);
    expect(readCfg8(cap1)).toBe(0x09);
    expect(readCfg8(cap1 + 2)).toBe(20);
    expect(readCfg8(cap1 + 3)).toBe(2); // notify cfg
    expect(readCfg8(cap1 + 4)).toBe(0);
    expect(readCfg32(cap1 + 8)).toBe(0x1000);
    expect(readCfg32(cap1 + 12)).toBe(0x0100);
    expect(readCfg32(cap1 + 16)).toBe(4); // notify_off_multiplier

    const cap2 = readCfg8(cap1 + 1);
    expect(cap2).toBe(0x64);
    expect(readCfg8(cap2)).toBe(0x09);
    expect(readCfg8(cap2 + 2)).toBe(16);
    expect(readCfg8(cap2 + 3)).toBe(3); // isr cfg
    expect(readCfg32(cap2 + 8)).toBe(0x2000);
    expect(readCfg32(cap2 + 12)).toBe(0x0020);

    const cap3 = readCfg8(cap2 + 1);
    expect(cap3).toBe(0x74);
    expect(readCfg8(cap3)).toBe(0x09);
    expect(readCfg8(cap3 + 2)).toBe(16);
    expect(readCfg8(cap3 + 3)).toBe(4); // device cfg
    expect(readCfg32(cap3 + 8)).toBe(0x3000);
    expect(readCfg32(cap3 + 12)).toBe(0x0100);
    expect(readCfg8(cap3 + 1)).toBe(0); // end of list

    dev?.destroy();
  });
});
