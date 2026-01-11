import { workerData } from "node:worker_threads";

import { openRingByKind } from "../../../ipc/ipc.ts";
import { queueKind } from "../../../ipc/layout.ts";
import { encodeEvent } from "../../../ipc/protocol.ts";
import { DeviceManager } from "../../device_manager.ts";
import type { IrqSink } from "../../device_manager.ts";
import { I8042Controller } from "../../devices/i8042.ts";
import { UART_COM1, Uart16550, type SerialOutputSink } from "../../devices/uart16550.ts";
import { AeroIpcIoServer } from "../aero_ipc_io.ts";

const { ipcBuffer, tickIntervalMs } = workerData as {
  ipcBuffer: SharedArrayBuffer;
  tickIntervalMs?: number;
};

const cmdQ = openRingByKind(ipcBuffer, queueKind.CMD);
const evtQ = openRingByKind(ipcBuffer, queueKind.EVT);

const irqSink: IrqSink = {
  raiseIrq: (irq) => evtQ.pushBlocking(encodeEvent({ kind: "irqRaise", irq: irq & 0xff })),
  lowerIrq: (irq) => evtQ.pushBlocking(encodeEvent({ kind: "irqLower", irq: irq & 0xff })),
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

const uart = new Uart16550(UART_COM1, serialSink);
mgr.registerPortIo(uart.basePort, uart.basePort + 7, uart);

new AeroIpcIoServer(cmdQ, evtQ, mgr, { tickIntervalMs }).run();
