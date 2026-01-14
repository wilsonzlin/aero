import { describe, expect, it, vi } from "vitest";

import type { IrqSink } from "../device_manager";
import { VirtioNetPciDevice, type VirtioNetPciBridgeLike } from "./virtio_net";

function readU16LE(buf: Uint8Array, off: number): number {
  return (buf[off]! | (buf[off + 1]! << 8)) >>> 0;
}

function readU32LE(buf: Uint8Array, off: number): number {
  return (buf[off]! | (buf[off + 1]! << 8) | (buf[off + 2]! << 16) | (buf[off + 3]! << 24)) >>> 0;
}

describe("io/devices/VirtioNetPciDevice", () => {
  it("writes subsystem IDs + virtio capability chain in initPciConfig()", () => {
    const bridge: VirtioNetPciBridgeLike = {
      mmio_read: vi.fn(() => 0),
      mmio_write: vi.fn(),
      tick: vi.fn(),
      irq_level: vi.fn(() => false),
      free: vi.fn(),
    };
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };
    const dev = new VirtioNetPciDevice({ bridge, irqSink });
    expect(dev.bdf).toEqual({ bus: 0, device: 8, function: 0 });

    const config = new Uint8Array(256);
    dev.initPciConfig(config);

    // Subsystem IDs.
    expect(readU16LE(config, 0x2c)).toBe(0x1af4);
    expect(readU16LE(config, 0x2e)).toBe(0x0001);

    // Status: capabilities list bit.
    expect((readU16LE(config, 0x06) & 0x0010) !== 0).toBe(true);
    // Capability pointer.
    expect(config[0x34]).toBe(0x50);

    // COMMON capability at 0x50.
    expect(config[0x50]).toBe(0x09);
    expect(config[0x51]).toBe(0x60);
    expect(config[0x52]).toBe(16);
    expect(config[0x53]).toBe(1);
    expect(config[0x54]).toBe(0);
    expect(readU32LE(config, 0x58)).toBe(0x0000);
    expect(readU32LE(config, 0x5c)).toBe(0x0100);

    // NOTIFY capability at 0x60.
    expect(config[0x60]).toBe(0x09);
    expect(config[0x61]).toBe(0x74);
    expect(config[0x62]).toBe(20);
    expect(config[0x63]).toBe(2);
    expect(config[0x64]).toBe(0);
    expect(readU32LE(config, 0x68)).toBe(0x1000);
    expect(readU32LE(config, 0x6c)).toBe(0x0100);
    expect(readU32LE(config, 0x70)).toBe(4);

    // ISR capability at 0x74.
    expect(config[0x74]).toBe(0x09);
    expect(config[0x75]).toBe(0x84);
    expect(config[0x76]).toBe(16);
    expect(config[0x77]).toBe(3);
    expect(config[0x78]).toBe(0);
    expect(readU32LE(config, 0x7c)).toBe(0x2000);
    expect(readU32LE(config, 0x80)).toBe(0x0020);

    // DEVICE capability at 0x84.
    expect(config[0x84]).toBe(0x09);
    expect(config[0x85]).toBe(0x00);
    expect(config[0x86]).toBe(16);
    expect(config[0x87]).toBe(4);
    expect(config[0x88]).toBe(0);
    expect(readU32LE(config, 0x8c)).toBe(0x3000);
    expect(readU32LE(config, 0x90)).toBe(0x0100);

    // Alignment + acyclic traversal.
    const visited = new Set<number>();
    let cap = config[0x34]!;
    while (cap !== 0) {
      expect(cap % 4).toBe(0);
      expect(visited.has(cap)).toBe(false);
      visited.add(cap);
      expect(config[cap]).toBe(0x09);
      cap = config[cap + 1]!;
    }
    expect(Array.from(visited)).toEqual([0x50, 0x60, 0x74, 0x84]);
  });

  it("accepts camelCase virtio-net bridge exports (backwards compatibility)", () => {
    const mmioRead = vi.fn(() => 0x1234_5678);
    const mmioWrite = vi.fn();
    const poll = vi.fn();
    const irqAsserted = vi.fn(() => false);
    const setPciCommand = vi.fn();
    const free = vi.fn();

    const bridge = {
      mmioRead,
      mmioWrite,
      poll,
      irqAsserted,
      setPciCommand,
      free,
    };
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };
    const dev = new VirtioNetPciDevice({ bridge: bridge as unknown as VirtioNetPciBridgeLike, irqSink });

    // Defined region (COMMON_CFG): should forward to bridge.
    expect(dev.mmioRead(0, 0x0000n, 4)).toBe(0x1234_5678);
    expect(mmioRead).toHaveBeenCalledWith(0, 4);

    dev.mmioWrite(0, 0x0000n, 4, 0xdead_beef);
    expect(mmioWrite).toHaveBeenCalledWith(0, 4, 0xdead_beef);

    // Enable bus mastering so poll runs, and ensure PCI command is mirrored.
    dev.onPciCommandWrite(1 << 2);
    expect(setPciCommand).toHaveBeenCalledWith(0x0004);

    dev.tick(0);
    expect(poll).toHaveBeenCalledTimes(1);

    dev.destroy();
    expect(free).toHaveBeenCalled();
  });

  it("returns 0 and ignores writes for undefined BAR0 MMIO offsets (contract v1)", () => {
    const mmioRead = vi.fn(() => 0x1234_5678);
    const mmioWrite = vi.fn();
    const irqLevel = vi.fn(() => false);

    const bridge: VirtioNetPciBridgeLike = {
      mmio_read: mmioRead,
      mmio_write: mmioWrite,
      poll: vi.fn(),
      irq_level: irqLevel,
      free: vi.fn(),
    };
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };
    const dev = new VirtioNetPciDevice({ bridge, irqSink });

    // Defined region (COMMON_CFG): should forward to bridge.
    expect(dev.mmioRead(0, 0x0000n, 4)).toBe(0x1234_5678);
    expect(mmioRead).toHaveBeenCalledWith(0, 4);

    mmioRead.mockClear();
    // Undefined region within BAR0: must read as 0 and must not hit the bridge.
    expect(dev.mmioRead(0, 0x0400n, 4)).toBe(0);
    expect(mmioRead).not.toHaveBeenCalled();

    // Crossing a defined region boundary counts as undefined for the requested width.
    expect(dev.mmioRead(0, 0x00ffn, 4)).toBe(0);

    // Undefined writes are ignored (no bridge call).
    dev.mmioWrite(0, 0x0400n, 4, 0xdead_beef);
    expect(mmioWrite).not.toHaveBeenCalled();

    // Defined writes are forwarded.
    dev.mmioWrite(0, 0x0000n, 4, 0xdead_beef);
    expect(mmioWrite).toHaveBeenCalledWith(0, 4, 0xdead_beef);
  });

  it("gates device polling on PCI Bus Master Enable (command bit 2)", () => {
    const poll = vi.fn();
    const bridge: VirtioNetPciBridgeLike = {
      mmio_read: vi.fn(() => 0),
      mmio_write: vi.fn(),
      poll,
      irq_asserted: vi.fn(() => false),
      free: vi.fn(),
    };
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };
    const dev = new VirtioNetPciDevice({ bridge, irqSink });

    // Not bus-master enabled by default; tick should not poll the device.
    dev.tick(0);
    expect(poll).not.toHaveBeenCalled();

    // Enable BME (bit 2).
    dev.onPciCommandWrite(1 << 2);
    dev.tick(1);
    expect(poll).toHaveBeenCalledTimes(1);
  });

  it("respects PCI command Interrupt Disable bit (bit 10) when syncing INTx level", () => {
    let irq = false;
    const bridge: VirtioNetPciBridgeLike = {
      mmio_read: vi.fn(() => 0),
      mmio_write: vi.fn(),
      irq_asserted: vi.fn(() => irq),
      free: vi.fn(),
    };
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };
    const dev = new VirtioNetPciDevice({ bridge, irqSink });

    // Start deasserted.
    dev.tick(0);
    expect(irqSink.raiseIrq).not.toHaveBeenCalled();

    // Assert line.
    irq = true;
    dev.tick(1);
    expect(irqSink.raiseIrq).toHaveBeenCalledTimes(1);
    expect(irqSink.raiseIrq).toHaveBeenCalledWith(10);

    // Disable INTx in PCI command register: should drop the line.
    dev.onPciCommandWrite(1 << 10);
    expect(irqSink.lowerIrq).toHaveBeenCalledTimes(1);
    expect(irqSink.lowerIrq).toHaveBeenCalledWith(10);

    // No additional edges while disabled.
    (irqSink.raiseIrq as ReturnType<typeof vi.fn>).mockClear();
    (irqSink.lowerIrq as ReturnType<typeof vi.fn>).mockClear();
    dev.tick(2);
    expect(irqSink.raiseIrq).not.toHaveBeenCalled();
    expect(irqSink.lowerIrq).not.toHaveBeenCalled();

    // Re-enable INTx: should reassert because the device-level condition is still true.
    dev.onPciCommandWrite(0);
    expect(irqSink.raiseIrq).toHaveBeenCalledTimes(1);
    expect(irqSink.raiseIrq).toHaveBeenCalledWith(10);
  });
});
