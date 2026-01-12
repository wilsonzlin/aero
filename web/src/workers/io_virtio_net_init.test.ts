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

    class FakeVirtioNetPciBridge {
      constructor(_guestBase: number, _guestSize: number, _ioIpc: SharedArrayBuffer) {}

      mmio_read(_offset: number, _size: number): number {
        return 0;
      }
      mmio_write(_offset: number, _size: number, _value: number): void {}
      io_read(_offset: number, _size: number): number {
        return 0;
      }
      io_write(_offset: number, _size: number, _value: number): void {}

      tick(): void {}
      irq_level(): boolean {
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

    // Read vendor/device IDs for device 0 on bus 0.
    mgr.portWrite(0x0cf8, 4, 0x8000_0000 | 0x00);
    const id = mgr.portRead(0x0cfc, 4) >>> 0;
    expect(id & 0xffff).toBe(0x1af4);
    expect((id >>> 16) & 0xffff).toBe(0x1000);

    // Probe BAR0 size mask via the standard all-ones write.
    mgr.portWrite(0x0cf8, 4, 0x8000_0000 | 0x10);
    mgr.portWrite(0x0cfc, 4, 0xffff_ffff);
    const mask = mgr.portRead(0x0cfc, 4) >>> 0;
    expect(mask).toBe(0xffff_c000);

    dev?.destroy();
  });
});

