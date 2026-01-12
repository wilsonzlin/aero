/// <reference lib="webworker" />

import { openRingByKind } from "../ipc/ipc.ts";
import { queueKind } from "../ipc/layout.ts";
import { encodeEvent } from "../ipc/protocol.ts";
import { DeviceManager } from "../io/device_manager.ts";
import { I8042Controller } from "../io/devices/i8042.ts";
import { PciTestDevice } from "../io/devices/pci_test_device.ts";
import { UART_COM1, Uart16550 } from "../io/devices/uart16550.ts";
import { AeroIpcIoServer } from "../io/ipc/aero_ipc_io.ts";
import type { IrqSink } from "../io/device_manager.ts";
import type { SerialOutputSink } from "../io/devices/uart16550.ts";

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
    // IRQ events are line level transitions (assert/deassert) transported over AIPC. Edge-triggered
    // devices must emit explicit pulses (`raiseIrq` then `lowerIrq`). See `docs/irq-semantics.md`.
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
      // Best-effort: serial output is useful for debug; if the ring is full,
      // don't stall device emulation.
      evtQ.tryPush(encodeEvent({ kind: "serialOutput", port: port & 0xffff, data }));
    },
  };

  const mgr = new DeviceManager(irqSink);

  if (devices.includes("i8042")) {
    const i8042 = new I8042Controller(mgr.irqSink, { systemControl });
    mgr.registerPortIo(0x0060, 0x0060, i8042);
    mgr.registerPortIo(0x0064, 0x0064, i8042);
  }

  if (devices.includes("pci_test")) {
    mgr.registerPciDevice(new PciTestDevice());
  }

  if (devices.includes("uart16550")) {
    const uart = new Uart16550(UART_COM1, serialSink);
    mgr.registerPortIo(uart.basePort, uart.basePort + 7, uart);
  }

  new AeroIpcIoServer(cmdQ, evtQ, mgr, { tickIntervalMs }).run();
};
