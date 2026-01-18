/// <reference lib="webworker" />

import { openRingByKind } from "../ipc/ipc.ts";
import { queueKind } from "../ipc/layout.ts";
import { encodeEvent } from "../ipc/protocol.ts";
import { DeviceManager } from "../io/device_manager.ts";
import { I8042Controller } from "../io/devices/i8042.ts";
import { PciMultifunctionTestDeviceFn0, PciMultifunctionTestDeviceFn1 } from "../io/devices/pci_multifunction_test_device.ts";
import { PciTestDevice } from "../io/devices/pci_test_device.ts";
import { UART_COM1, Uart16550 } from "../io/devices/uart16550.ts";
import { AeroIpcIoServer } from "../io/ipc/aero_ipc_io.ts";
import { IRQ_REFCOUNT_ASSERT, IRQ_REFCOUNT_DEASSERT, IRQ_REFCOUNT_SATURATED, IRQ_REFCOUNT_UNDERFLOW, applyIrqRefCountChange } from "../io/irq_refcount.ts";
import type { IrqSink } from "../io/device_manager.ts";
import type { SerialOutputSink } from "../io/devices/uart16550.ts";
import { parseIoAipcWorkerInitMessage } from "./worker_init_parsers.ts";

const IS_DEV = (import.meta as { env?: { DEV?: boolean } }).env?.DEV === true;

export type IoAipcWorkerInitMessage = {
  type: "init";
  ipcBuffer: SharedArrayBuffer;
  cmdKind?: number;
  evtKind?: number;
  tickIntervalMs?: number;
  devices?: string[];
};

const ctx = globalThis as unknown as DedicatedWorkerGlobalScope;

ctx.onmessage = (ev: MessageEvent<IoAipcWorkerInitMessage>) => {
  const init = parseIoAipcWorkerInitMessage(ev.data);
  if (!init) return;
  const { ipcBuffer, cmdKind, evtKind, tickIntervalMs, devices } = init;

  const cmdQ = openRingByKind(ipcBuffer, cmdKind);
  const evtQ = openRingByKind(ipcBuffer, evtKind);

  // IRQ delivery models physical line levels with refcounted wire-OR semantics
  // (see `docs/irq-semantics.md`). Emit only effective line transitions:
  //  - `irqRaise` on 0→1
  //  - `irqLower` on 1→0
  //
  // This matches the canonical browser IO worker implementation.
  const irqRefCounts = new Uint16Array(256);
  const irqWarnedUnderflow = new Uint8Array(256);
  const irqWarnedSaturated = new Uint8Array(256);
  const irqSink: IrqSink = {
    raiseIrq: (irq) => {
      const idx = irq & 0xff;
      const flags = applyIrqRefCountChange(irqRefCounts, idx, true);
      if (flags & IRQ_REFCOUNT_ASSERT) {
        evtQ.pushBlocking(encodeEvent({ kind: "irqRaise", irq: idx }));
      }
      if (IS_DEV && (flags & IRQ_REFCOUNT_SATURATED) && irqWarnedSaturated[idx] === 0) {
        irqWarnedSaturated[idx] = 1;
        console.warn(`[io_aipc.worker] IRQ${idx} refcount saturated at 0xffff (raiseIrq without matching lowerIrq?)`);
      }
    },
    lowerIrq: (irq) => {
      const idx = irq & 0xff;
      const flags = applyIrqRefCountChange(irqRefCounts, idx, false);
      if (flags & IRQ_REFCOUNT_DEASSERT) {
        evtQ.pushBlocking(encodeEvent({ kind: "irqLower", irq: idx }));
      }
      if (IS_DEV && (flags & IRQ_REFCOUNT_UNDERFLOW) && irqWarnedUnderflow[idx] === 0) {
        irqWarnedUnderflow[idx] = 1;
        console.warn(`[io_aipc.worker] IRQ${idx} refcount underflow (lowerIrq while already deasserted)`);
      }
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

  if (devices.includes("pci_multifn_test")) {
    const fn0 = mgr.registerPciDevice(new PciMultifunctionTestDeviceFn0());
    mgr.registerPciDevice(new PciMultifunctionTestDeviceFn1(), { device: fn0.device, function: 1 });
  }

  if (devices.includes("uart16550")) {
    const uart = new Uart16550(UART_COM1, serialSink);
    mgr.registerPortIo(uart.basePort, uart.basePort + 7, uart);
  }

  new AeroIpcIoServer(cmdQ, evtQ, mgr, { tickIntervalMs }).run();
};
