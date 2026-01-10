/// <reference lib="webworker" />

import { openRingByKind } from "../ipc/ipc.ts";
import { queueKind } from "../ipc/layout.ts";
import { encodeEvent } from "../ipc/protocol.ts";
import { DeviceManager } from "../io/device_manager.ts";
import { I8042Controller } from "../io/devices/i8042.ts";
import { PciTestDevice } from "../io/devices/pci_test_device.ts";
import { AeroIpcIoServer } from "../io/ipc/aero_ipc_io.ts";
import type { IrqSink } from "../io/device_manager.ts";

export type IoAipcWorkerInitMessage = {
  type: "init";
  ipcBuffer: SharedArrayBuffer;
  cmdKind?: number;
  evtKind?: number;
  tickIntervalMs?: number;
  devices?: string[];
};

const ctx = self as unknown as DedicatedWorkerGlobalScope;

ctx.onmessage = (ev: MessageEvent<IoAipcWorkerInitMessage>) => {
  if (ev.data?.type !== "init") return;
  const {
    ipcBuffer,
    cmdKind = queueKind.CMD,
    evtKind = queueKind.EVT,
    tickIntervalMs = 5,
    devices = ["i8042"],
  } = ev.data;

  const cmdQ = openRingByKind(ipcBuffer, cmdKind);
  const evtQ = openRingByKind(ipcBuffer, evtKind);

  const irqSink: IrqSink = {
    raiseIrq: (irq) => evtQ.pushBlocking(encodeEvent({ kind: "irqRaise", irq: irq & 0xff })),
    lowerIrq: (irq) => evtQ.pushBlocking(encodeEvent({ kind: "irqLower", irq: irq & 0xff })),
  };

  const mgr = new DeviceManager(irqSink);

  if (devices.includes("i8042")) {
    const i8042 = new I8042Controller(mgr.irqSink);
    mgr.registerPortIo(0x0060, 0x0060, i8042);
    mgr.registerPortIo(0x0064, 0x0064, i8042);
  }

  if (devices.includes("pci_test")) {
    mgr.registerPciDevice(new PciTestDevice());
  }

  new AeroIpcIoServer(cmdQ, evtQ, mgr, { tickIntervalMs }).run();
};

