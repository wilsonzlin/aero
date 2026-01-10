import { workerData } from "node:worker_threads";

import { RingBuffer } from "../../src/ipc/ring_buffer.ts";
import { encodeEvent } from "../../src/ipc/protocol.ts";
import { DeviceManager } from "../../src/io/device_manager.ts";
import { I8042Controller } from "../../src/io/devices/i8042.ts";
import { PciTestDevice } from "../../src/io/devices/pci_test_device.ts";
import { AeroIpcIoServer } from "../../src/io/ipc/aero_ipc_io.ts";
import type { IrqSink } from "../../src/io/device_manager.ts";

const { sab, cmdOffset, evtOffset, devices, tickIntervalMs } = workerData as {
  sab: SharedArrayBuffer;
  cmdOffset: number;
  evtOffset: number;
  devices?: string[];
  tickIntervalMs?: number;
};

const cmdQ = new RingBuffer(sab, cmdOffset);
const evtQ = new RingBuffer(sab, evtOffset);

const irqSink: IrqSink = {
  raiseIrq: (irq) => evtQ.pushBlocking(encodeEvent({ kind: "irqRaise", irq: irq & 0xff })),
  lowerIrq: (irq) => evtQ.pushBlocking(encodeEvent({ kind: "irqLower", irq: irq & 0xff })),
};

const mgr = new DeviceManager(irqSink);

const enabled = devices ?? ["i8042"];
if (enabled.includes("i8042")) {
  const i8042 = new I8042Controller(mgr.irqSink);
  mgr.registerPortIo(0x0060, 0x0060, i8042);
  mgr.registerPortIo(0x0064, 0x0064, i8042);
}
if (enabled.includes("pci_test")) {
  mgr.registerPciDevice(new PciTestDevice());
}

new AeroIpcIoServer(cmdQ, evtQ, mgr, { tickIntervalMs }).run();

