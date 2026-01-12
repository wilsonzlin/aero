import { workerData } from "node:worker_threads";

import { openRingByKind } from "../../../ipc/ipc.ts";
import { queueKind } from "../../../ipc/layout.ts";
import { encodeEvent } from "../../../ipc/protocol.ts";
import { DeviceManager } from "../../device_manager.ts";
import { IRQ_REFCOUNT_ASSERT, IRQ_REFCOUNT_DEASSERT, applyIrqRefCountChange } from "../../irq_refcount.ts";
import type { IrqSink } from "../../device_manager.ts";
import type { MmioHandler } from "../../bus/mmio.ts";
import { I8042Controller } from "../../devices/i8042.ts";
import { PciTestDevice } from "../../devices/pci_test_device.ts";
import { UART_COM1, Uart16550, type SerialOutputSink } from "../../devices/uart16550.ts";
import { AeroIpcIoServer } from "../aero_ipc_io.ts";

const { ipcBuffer, tickIntervalMs } = workerData as {
  ipcBuffer: SharedArrayBuffer;
  tickIntervalMs?: number;
};

const cmdQ = openRingByKind(ipcBuffer, queueKind.CMD);
const evtQ = openRingByKind(ipcBuffer, queueKind.EVT);

// Match the browser runtime IRQ contract: refcounted wire-OR level transitions.
// Emit only 0→1 (`irqRaise`) and 1→0 (`irqLower`) transitions.
const irqRefCounts = new Uint16Array(256);
const irqSink: IrqSink = {
  raiseIrq: (irq) => {
    const idx = irq & 0xff;
    const flags = applyIrqRefCountChange(irqRefCounts, idx, true);
    if (flags & IRQ_REFCOUNT_ASSERT) evtQ.pushBlocking(encodeEvent({ kind: "irqRaise", irq: idx }));
  },
  lowerIrq: (irq) => {
    const idx = irq & 0xff;
    const flags = applyIrqRefCountChange(irqRefCounts, idx, false);
    if (flags & IRQ_REFCOUNT_DEASSERT) evtQ.pushBlocking(encodeEvent({ kind: "irqLower", irq: idx }));
  },
};

const systemControl = {
  setA20: (enabled: boolean) => {
    evtQ.pushBlocking(encodeEvent({ kind: "a20Set", enabled: Boolean(enabled) }));
  },
  requestReset: () => {
    evtQ.pushBlocking(encodeEvent({ kind: "resetRequest" }));
  },
};

const serialSink: SerialOutputSink = {
  write: (port, data) => {
    evtQ.pushBlocking(encodeEvent({ kind: "serialOutput", port: port & 0xffff, data }));
  },
};

const mgr = new DeviceManager(irqSink);
const i8042 = new I8042Controller(mgr.irqSink, { systemControl });
mgr.registerPortIo(0x0060, 0x0060, i8042);
mgr.registerPortIo(0x0064, 0x0064, i8042);

mgr.registerPciDevice(new PciTestDevice());

const uart = new Uart16550(UART_COM1, serialSink);
mgr.registerPortIo(uart.basePort, uart.basePort + 7, uart);

class MmioTestDevice implements MmioHandler {
  #reg0 = 0;

  mmioRead(offset: bigint, size: number): number {
    if (offset === 0n && size === 4) return this.#reg0 >>> 0;
    // Default unmapped reads are all-ones.
    return size === 1 ? 0xff : size === 2 ? 0xffff : 0xffff_ffff;
  }

  mmioWrite(offset: bigint, size: number, value: number): void {
    if (offset === 0n && size === 4) this.#reg0 = value >>> 0;
  }
}

// Reserve a fixed MMIO range for the integration test.
mgr.registerMmio(0x1000_0000n, 0x100n, new MmioTestDevice());

new AeroIpcIoServer(cmdQ, evtQ, mgr, { tickIntervalMs }).run();
