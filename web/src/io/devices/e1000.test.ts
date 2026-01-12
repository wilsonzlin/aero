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

  it("clears any pending host-side TX frame and re-syncs the IRQ level on snapshot restore", () => {
    // Capacity 8 bytes: enough for a single 1-byte payload record
    // (len=1 => record size alignUp(4+1,4)=8).
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
    let irq = false;
    const bridge: E1000BridgeLike = {
      mmio_read: vi.fn(() => 0),
      mmio_write: vi.fn(),
      io_read: vi.fn(() => 0),
      io_write: vi.fn(),
      poll: vi.fn(),
      receive_frame: vi.fn(),
      pop_tx_frame: vi.fn(() => txQueue.shift()),
      irq_level: vi.fn(() => irq),
      free: vi.fn(),
    };
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };

    const dev = new E1000PciDevice({ bridge, irqSink, netTxRing: netTx, netRxRing: netRx });

    dev.tick(0);
    expect(bridge.pop_tx_frame).toHaveBeenCalledTimes(1);

    // Snapshot restore should clear transient state and re-drive the INTx level.
    irq = true;
    dev.onSnapshotRestore();
    expect(irqSink.raiseIrq).toHaveBeenCalledTimes(1);
    expect(irqSink.raiseIrq).toHaveBeenCalledWith(10);

    // Consume the old entry so the ring can accept a new frame.
    expect(Array.from(netTx.tryPop()!)).toEqual([0x00]);

    dev.tick(1);
    // `E1000PciDevice` may call `pop_tx_frame()` one extra time to detect the end of the TX queue
    // (i.e. it pops until null/undefined).
    expect(bridge.pop_tx_frame).toHaveBeenCalledTimes(3);
    expect(Array.from(netTx.tryPop()!)).toEqual([0x02]);
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
});
