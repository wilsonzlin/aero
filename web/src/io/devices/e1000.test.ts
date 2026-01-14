import { describe, expect, it, vi } from "vitest";

import { createIpcBuffer, openRingByKind } from "../../ipc/ipc";
import { IO_IPC_NET_RX_QUEUE_KIND, IO_IPC_NET_TX_QUEUE_KIND } from "../../runtime/shared_layout";
import type { IrqSink } from "../device_manager";
import { MmioBus } from "../bus/mmio";
import { PciBus } from "../bus/pci";
import { PortIoBus } from "../bus/portio";
import { E1000PciDevice, type E1000BridgeLike } from "./e1000";

function cfgAddr(dev: number, fn: number, off: number): number {
  // PCI config mechanism #1 (I/O ports 0xCF8/0xCFC).
  return (0x8000_0000 | ((dev & 0x1f) << 11) | ((fn & 0x07) << 8) | (off & 0xfc)) >>> 0;
}

function makeCfgIo(portBus: PortIoBus) {
  return {
    writeU16(dev: number, fn: number, off: number, value: number): void {
      portBus.write(0x0cf8, 4, cfgAddr(dev, fn, off));
      portBus.write(0x0cfc + (off & 3), 2, value & 0xffff);
    },
  };
}

describe("io/devices/E1000PciDevice", () => {
  it("exposes the expected PCI identity and BAR layout", () => {
    const { buffer } = createIpcBuffer([
      { kind: IO_IPC_NET_TX_QUEUE_KIND, capacityBytes: 256 },
      { kind: IO_IPC_NET_RX_QUEUE_KIND, capacityBytes: 256 },
    ]);
    const netTx = openRingByKind(buffer, IO_IPC_NET_TX_QUEUE_KIND);
    const netRx = openRingByKind(buffer, IO_IPC_NET_RX_QUEUE_KIND);

    const bridge: E1000BridgeLike = {
      mmio_read: vi.fn(() => 0),
      mmio_write: vi.fn(),
      io_read: vi.fn(() => 0),
      io_write: vi.fn(),
      poll: vi.fn(),
      receive_frame: vi.fn(),
      pop_tx_frame: vi.fn(() => undefined),
      irq_level: vi.fn(() => false),
      free: vi.fn(),
    };
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };

    const dev = new E1000PciDevice({ bridge, irqSink, netTxRing: netTx, netRxRing: netRx });
    expect(dev.bdf).toEqual({ bus: 0, device: 5, function: 0 });
    expect(dev.vendorId).toBe(0x8086);
    expect(dev.deviceId).toBe(0x100e);
    expect(dev.classCode).toBe(0x02_00_00);
    expect(dev.revisionId).toBe(0);
    expect(dev.irqLine).toBe(10);
    expect(dev.bars).toEqual([{ kind: "mmio32", size: 0x20_000 }, { kind: "io", size: 0x40 }, null, null, null, null]);
  });

  it("accepts camelCase E1000 bridge exports (backwards compatibility)", () => {
    const { buffer } = createIpcBuffer([
      { kind: IO_IPC_NET_TX_QUEUE_KIND, capacityBytes: 256 },
      { kind: IO_IPC_NET_RX_QUEUE_KIND, capacityBytes: 256 },
    ]);
    const netTx = openRingByKind(buffer, IO_IPC_NET_TX_QUEUE_KIND);
    const netRx = openRingByKind(buffer, IO_IPC_NET_RX_QUEUE_KIND);

    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };
    const mmioRead = vi.fn(() => 0);
    const mmioWrite = vi.fn();
    const ioRead = vi.fn(() => 0);
    const ioWrite = vi.fn();
    const poll = vi.fn();
    const receiveFrame = vi.fn();
    const popTxFrame = vi.fn(() => undefined);
    const irqLevel = vi.fn(() => false);
    const setPciCommand = vi.fn();
    const free = vi.fn();

    // Simulate a WASM build (or manual shim) that exposes camelCase methods.
    const bridge = {
      mmioRead,
      mmioWrite,
      ioRead,
      ioWrite,
      poll,
      receiveFrame,
      popTxFrame,
      irqLevel,
      setPciCommand,
      free,
    };

    const dev = new E1000PciDevice({ bridge: bridge as unknown as E1000BridgeLike, irqSink, netTxRing: netTx, netRxRing: netRx });

    dev.mmioRead(0, 0n, 4);
    expect(mmioRead).toHaveBeenCalledWith(0, 4);
    dev.mmioWrite(0, 0n, 4, 0x1234);
    expect(mmioWrite).toHaveBeenCalledWith(0, 4, 0x1234);

    dev.ioRead(1, 0, 4);
    expect(ioRead).toHaveBeenCalledWith(0, 4);
    dev.ioWrite(1, 0, 4, 0xfeed_beef);
    expect(ioWrite).toHaveBeenCalledWith(0, 4, 0xfeed_beef);

    // Enable bus mastering and ensure PCI command is mirrored.
    dev.onPciCommandWrite?.(0x1_0004);
    expect(setPciCommand).toHaveBeenCalledWith(0x0004);

    // NET_RX -> receiveFrame, and poll() should run when BME is enabled.
    expect(netRx.tryPush(new Uint8Array([1, 2, 3]))).toBe(true);
    dev.tick(0);
    expect(receiveFrame).toHaveBeenCalledTimes(1);
    expect(Array.from(receiveFrame.mock.calls[0]![0] as Uint8Array)).toEqual([1, 2, 3]);
    expect(poll).toHaveBeenCalledTimes(1);

    dev.destroy();
    expect(free).toHaveBeenCalled();
  });

  it("pumps NET_RX -> receive_frame and drains pop_tx_frame -> NET_TX", () => {
    const { buffer } = createIpcBuffer([
      { kind: IO_IPC_NET_TX_QUEUE_KIND, capacityBytes: 256 },
      { kind: IO_IPC_NET_RX_QUEUE_KIND, capacityBytes: 256 },
    ]);
    const netTx = openRingByKind(buffer, IO_IPC_NET_TX_QUEUE_KIND);
    const netRx = openRingByKind(buffer, IO_IPC_NET_RX_QUEUE_KIND);

    const txQueue: Uint8Array[] = [new Uint8Array([0xaa, 0xbb]), new Uint8Array([0xcc])];
    const receiveFrame = vi.fn();

    const bridge: E1000BridgeLike = {
      mmio_read: vi.fn(() => 0),
      mmio_write: vi.fn(),
      io_read: vi.fn(() => 0),
      io_write: vi.fn(),
      poll: vi.fn(),
      receive_frame: receiveFrame,
      pop_tx_frame: vi.fn(() => txQueue.shift()),
      irq_level: vi.fn(() => false),
      free: vi.fn(),
    };
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };

    const dev = new E1000PciDevice({ bridge, irqSink, netTxRing: netTx, netRxRing: netRx });

    // Host -> guest
    expect(netRx.tryPush(new Uint8Array([1, 2, 3]))).toBe(true);
    expect(netRx.tryPush(new Uint8Array([4, 5]))).toBe(true);

    dev.tick(0);

    expect(receiveFrame).toHaveBeenCalledTimes(2);
    expect(Array.from(receiveFrame.mock.calls[0]![0] as Uint8Array)).toEqual([1, 2, 3]);
    expect(Array.from(receiveFrame.mock.calls[1]![0] as Uint8Array)).toEqual([4, 5]);

    // Guest -> host
    expect(Array.from(netTx.tryPop()!)).toEqual([0xaa, 0xbb]);
    expect(Array.from(netTx.tryPop()!)).toEqual([0xcc]);
    expect(netTx.tryPop()).toBe(null);
  });

  it("keeps at most one pending TX frame when NET_TX is full", () => {
    // Capacity 8 bytes: enough for a single 1-byte payload record
    // (len=1 => record size alignUp(4+1,8)=8).
    const { buffer } = createIpcBuffer([
      { kind: IO_IPC_NET_TX_QUEUE_KIND, capacityBytes: 8 },
      { kind: IO_IPC_NET_RX_QUEUE_KIND, capacityBytes: 256 },
    ]);
    const netTx = openRingByKind(buffer, IO_IPC_NET_TX_QUEUE_KIND);
    const netRx = openRingByKind(buffer, IO_IPC_NET_RX_QUEUE_KIND);

    // Fill the ring so pushes fail.
    expect(netTx.tryPush(new Uint8Array([0x00]))).toBe(true);

    const txQueue: Uint8Array[] = [new Uint8Array([0x01]), new Uint8Array([0x02])];
    const bridge: E1000BridgeLike = {
      mmio_read: vi.fn(() => 0),
      mmio_write: vi.fn(),
      io_read: vi.fn(() => 0),
      io_write: vi.fn(),
      poll: vi.fn(),
      receive_frame: vi.fn(),
      pop_tx_frame: vi.fn(() => txQueue.shift()),
      irq_level: vi.fn(() => false),
      free: vi.fn(),
    };
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };

    const dev = new E1000PciDevice({ bridge, irqSink, netTxRing: netTx, netRxRing: netRx });

    dev.tick(0);
    expect(bridge.pop_tx_frame).toHaveBeenCalledTimes(1);

    // Ring is still full, so the device should not keep popping more frames.
    dev.tick(1);
    expect(bridge.pop_tx_frame).toHaveBeenCalledTimes(1);

    // Consume the old entry so the pending frame can flush.
    expect(Array.from(netTx.tryPop()!)).toEqual([0x00]);
    dev.tick(2);

    expect(Array.from(netTx.tryPop()!)).toEqual([0x01]);
  });

  it("gates device polling on PCI Bus Master Enable (command bit 2)", () => {
    const { buffer } = createIpcBuffer([
      { kind: IO_IPC_NET_TX_QUEUE_KIND, capacityBytes: 256 },
      { kind: IO_IPC_NET_RX_QUEUE_KIND, capacityBytes: 256 },
    ]);
    const netTx = openRingByKind(buffer, IO_IPC_NET_TX_QUEUE_KIND);
    const netRx = openRingByKind(buffer, IO_IPC_NET_RX_QUEUE_KIND);

    const poll = vi.fn();
    const bridge: E1000BridgeLike = {
      mmio_read: vi.fn(() => 0),
      mmio_write: vi.fn(),
      io_read: vi.fn(() => 0),
      io_write: vi.fn(),
      poll,
      receive_frame: vi.fn(),
      pop_tx_frame: vi.fn(() => undefined),
      irq_level: vi.fn(() => false),
      free: vi.fn(),
    };
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };

    const dev = new E1000PciDevice({ bridge, irqSink, netTxRing: netTx, netRxRing: netRx });

    // Not bus-master enabled by default; tick should not poll the device.
    dev.tick(0);
    expect(poll).not.toHaveBeenCalled();

    // Enable BME (bit 2).
    dev.onPciCommandWrite?.(1 << 2);
    dev.tick(1);
    expect(poll).toHaveBeenCalledTimes(1);
  });

  it("clears pending host-side TX and re-syncs IRQ level on snapshot restore", () => {
    // Capacity 8 bytes: enough for a single 1-byte payload record
    // (len=1 => record size alignUp(4+1,8)=8).
    const { buffer } = createIpcBuffer([
      { kind: IO_IPC_NET_TX_QUEUE_KIND, capacityBytes: 8 },
      { kind: IO_IPC_NET_RX_QUEUE_KIND, capacityBytes: 256 },
    ]);
    const netTx = openRingByKind(buffer, IO_IPC_NET_TX_QUEUE_KIND);
    const netRx = openRingByKind(buffer, IO_IPC_NET_RX_QUEUE_KIND);

    // Fill the ring so pushes fail.
    expect(netTx.tryPush(new Uint8Array([0x00]))).toBe(true);

    // First frame becomes pending (ring full); second frame should be the next one flushed after
    // snapshot restore because the pending host-side buffer is intentionally cleared.
    const txQueue: Uint8Array[] = [new Uint8Array([0x01]), new Uint8Array([0x02])];
    let irq = true;
    const popTxFrame = vi.fn(() => txQueue.shift());
    const bridge: E1000BridgeLike = {
      mmio_read: vi.fn(() => 0),
      mmio_write: vi.fn(),
      io_read: vi.fn(() => 0),
      io_write: vi.fn(),
      poll: vi.fn(),
      receive_frame: vi.fn(),
      pop_tx_frame: popTxFrame,
      irq_level: vi.fn(() => irq),
      free: vi.fn(),
    };

    const irqEvents: string[] = [];
    const irqSink: IrqSink = {
      raiseIrq: vi.fn((line) => irqEvents.push(`raise:${line}`)),
      lowerIrq: vi.fn((line) => irqEvents.push(`lower:${line}`)),
    };

    const dev = new E1000PciDevice({ bridge, irqSink, netTxRing: netTx, netRxRing: netRx });

    dev.tick(0);
    expect(popTxFrame).toHaveBeenCalledTimes(1);
    expect(irqEvents).toEqual(["raise:10"]);

    // Snapshot restore should clear transient state and re-drive the INTx level.
    dev.onSnapshotRestore();
    expect(irqEvents).toEqual(["raise:10", "lower:10", "raise:10"]);

    // Consume the old entry so the ring can accept a new frame.
    expect(Array.from(netTx.tryPop()!)).toEqual([0x00]);

    dev.tick(1);
    // `E1000PciDevice` drains TX by popping until `null`/`undefined`. Depending on implementation
    // details, it may probe for an additional frame after flushing. Avoid depending on an exact
    // call count; assert it advanced beyond the initial "pending" pop.
    expect(popTxFrame.mock.calls.length).toBeGreaterThanOrEqual(2);
    expect(Array.from(netTx.tryPop()!)).toEqual([0x02]);
    expect(netTx.tryPop()).toBe(null);
  });

  it("treats PCI INTx as a level-triggered IRQ and only emits transitions on edges", () => {
    const { buffer } = createIpcBuffer([
      { kind: IO_IPC_NET_TX_QUEUE_KIND, capacityBytes: 4096 },
      { kind: IO_IPC_NET_RX_QUEUE_KIND, capacityBytes: 4096 },
    ]);
    const netTx = openRingByKind(buffer, IO_IPC_NET_TX_QUEUE_KIND);
    const netRx = openRingByKind(buffer, IO_IPC_NET_RX_QUEUE_KIND);

    let irq = false;
    const bridge: E1000BridgeLike = {
      mmio_read: vi.fn(() => 0),
      mmio_write: vi.fn(),
      io_read: vi.fn(() => 0),
      io_write: vi.fn(),
      poll: vi.fn(),
      receive_frame: vi.fn(),
      pop_tx_frame: vi.fn(() => undefined),
      irq_level: vi.fn(() => irq),
      free: vi.fn(),
    };
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };

    const dev = new E1000PciDevice({ bridge, irqSink, netTxRing: netTx, netRxRing: netRx });

    dev.tick(0);
    expect(irqSink.raiseIrq).not.toHaveBeenCalled();

    irq = true;
    dev.mmioWrite(0, 0n, 4, 0x1234);
    expect(irqSink.raiseIrq).toHaveBeenCalledTimes(1);
    expect(irqSink.raiseIrq).toHaveBeenCalledWith(10);

    // No additional edge while asserted.
    dev.mmioWrite(0, 4n, 4, 0x5678);
    expect(irqSink.raiseIrq).toHaveBeenCalledTimes(1);

    irq = false;
    dev.tick(1);
    expect(irqSink.lowerIrq).toHaveBeenCalledTimes(1);
    expect(irqSink.lowerIrq).toHaveBeenCalledWith(10);
  });

  it("forwards PCI config writes (e.g. command/BME) to the WASM bridge when available", () => {
    const { buffer } = createIpcBuffer([
      { kind: IO_IPC_NET_TX_QUEUE_KIND, capacityBytes: 256 },
      { kind: IO_IPC_NET_RX_QUEUE_KIND, capacityBytes: 256 },
    ]);
    const netTx = openRingByKind(buffer, IO_IPC_NET_TX_QUEUE_KIND);
    const netRx = openRingByKind(buffer, IO_IPC_NET_RX_QUEUE_KIND);

    const pciConfigWrite = vi.fn();
    const setPciCommand = vi.fn();
    const bridge: E1000BridgeLike = {
      pci_config_write: pciConfigWrite,
      mmio_read: vi.fn(() => 0),
      mmio_write: vi.fn(),
      io_read: vi.fn(() => 0),
      io_write: vi.fn(),
      set_pci_command: setPciCommand,
      poll: vi.fn(),
      receive_frame: vi.fn(),
      pop_tx_frame: vi.fn(() => undefined),
      irq_level: vi.fn(() => false),
      free: vi.fn(),
    };
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };

    const dev = new E1000PciDevice({ bridge, irqSink, netTxRing: netTx, netRxRing: netRx });

    const portBus = new PortIoBus();
    const mmioBus = new MmioBus();
    const pciBus = new PciBus(portBus, mmioBus);
    pciBus.registerToPortBus();
    pciBus.registerDevice(dev, { device: 0, function: 0 });

    // PCI command register write: set Bus Master Enable (bit 2).
    const cfg = makeCfgIo(portBus);
    cfg.writeU16(0, 0, 0x04, 0x0004);

    expect(pciConfigWrite).toHaveBeenCalledTimes(1);
    // The PCI bus callback is invoked on the aligned dword (0x04..0x07).
    expect(pciConfigWrite).toHaveBeenCalledWith(0x04, 4, 0x0004);
    expect(setPciCommand).toHaveBeenCalledTimes(1);
    expect(setPciCommand).toHaveBeenCalledWith(0x0004);
  });

  it("gates INTx assertion when COMMAND.INTX_DISABLE is set", () => {
    const { buffer } = createIpcBuffer([
      { kind: IO_IPC_NET_TX_QUEUE_KIND, capacityBytes: 256 },
      { kind: IO_IPC_NET_RX_QUEUE_KIND, capacityBytes: 256 },
    ]);
    const netTx = openRingByKind(buffer, IO_IPC_NET_TX_QUEUE_KIND);
    const netRx = openRingByKind(buffer, IO_IPC_NET_RX_QUEUE_KIND);

    let irq = true;
    const bridge: E1000BridgeLike = {
      mmio_read: vi.fn(() => 0),
      mmio_write: vi.fn(),
      io_read: vi.fn(() => 0),
      io_write: vi.fn(),
      poll: vi.fn(),
      receive_frame: vi.fn(),
      pop_tx_frame: vi.fn(() => undefined),
      irq_level: vi.fn(() => irq),
      free: vi.fn(),
    };
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };

    const dev = new E1000PciDevice({ bridge, irqSink, netTxRing: netTx, netRxRing: netRx });

    // With INTx disabled, the wrapper must not assert the line.
    dev.onPciCommandWrite?.(1 << 10);
    expect(irqSink.raiseIrq).not.toHaveBeenCalled();

    // Re-enable INTx: since the device is still asserting, we should now raise.
    dev.onPciCommandWrite?.(0);
    expect(irqSink.raiseIrq).toHaveBeenCalledTimes(1);
    expect(irqSink.raiseIrq).toHaveBeenCalledWith(10);

    // Disable INTx again: line should drop.
    dev.onPciCommandWrite?.(1 << 10);
    expect(irqSink.lowerIrq).toHaveBeenCalledTimes(1);
    expect(irqSink.lowerIrq).toHaveBeenCalledWith(10);
  });
});
