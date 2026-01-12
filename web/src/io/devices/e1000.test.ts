import { describe, expect, it, vi } from "vitest";

import { createIpcBuffer, openRingByKind } from "../../ipc/ipc";
import { IO_IPC_NET_RX_QUEUE_KIND, IO_IPC_NET_TX_QUEUE_KIND } from "../../runtime/shared_layout";
import type { IrqSink } from "../device_manager";
import { E1000PciDevice, type E1000BridgeLike } from "./e1000";

describe("io/devices/E1000PciDevice", () => {
  it("forwards TX frames to NET_TX and delivers NET_RX frames to receive_frame()", () => {
    const { buffer } = createIpcBuffer([
      { kind: IO_IPC_NET_TX_QUEUE_KIND, capacityBytes: 4096 },
      { kind: IO_IPC_NET_RX_QUEUE_KIND, capacityBytes: 4096 },
    ]);
    const netTx = openRingByKind(buffer, IO_IPC_NET_TX_QUEUE_KIND);
    const netRx = openRingByKind(buffer, IO_IPC_NET_RX_QUEUE_KIND);

    const txFrames: Uint8Array[] = [new Uint8Array([0x01, 0x02, 0x03]), new Uint8Array([0xaa, 0xbb])];
    const receiveFrame = vi.fn();

    const bridge: E1000BridgeLike = {
      mmio_read: vi.fn(() => 0),
      mmio_write: vi.fn(),
      io_read: vi.fn(() => 0),
      io_write: vi.fn(),
      poll: vi.fn(),
      receive_frame: receiveFrame,
      pop_tx_frame: vi.fn(() => txFrames.shift()),
      irq_level: vi.fn(() => false),
      free: vi.fn(),
    };
    const irqSink: IrqSink = { raiseIrq: vi.fn(), lowerIrq: vi.fn() };

    // Preload a host->guest frame.
    expect(netRx.tryPush(new Uint8Array([0xde, 0xad, 0xbe, 0xef]))).toBe(true);

    const dev = new E1000PciDevice({ bridge, irqSink, netTxRing: netTx, netRxRing: netRx });
    dev.tick(0);

    // TX frames are drained into NET_TX.
    const out1 = netTx.tryPop();
    const out2 = netTx.tryPop();
    expect(out1 && Array.from(out1)).toEqual([0x01, 0x02, 0x03]);
    expect(out2 && Array.from(out2)).toEqual([0xaa, 0xbb]);
    expect(netTx.tryPop()).toBeNull();

    // NET_RX frame is delivered to the bridge.
    expect(receiveFrame).toHaveBeenCalledTimes(1);
    expect(Array.from(receiveFrame.mock.calls[0]![0] as Uint8Array)).toEqual([0xde, 0xad, 0xbe, 0xef]);
  });

  it("raises/lowers IRQ only on edges", () => {
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
});

