import { DeviceManager } from "../io/device_manager.ts";
import { I8042Controller } from "../io/devices/i8042.ts";
import { PciTestDevice } from "../io/devices/pci_test_device.ts";
import { UART_COM1, Uart16550 } from "../io/devices/uart16550.ts";
import { IoServer } from "../io/ipc/io_server.ts";
import { SharedRingBuffer } from "../io/ipc/ring_buffer.ts";
import {
  IO_MESSAGE_STRIDE_U32,
  IO_OP_A20_SET,
  IO_OP_IRQ_LOWER,
  IO_OP_IRQ_RAISE,
  IO_OP_RESET_REQUEST,
  IO_OP_SERIAL_OUT,
  writeIoMessage,
} from "../io/ipc/io_protocol.ts";
import { IRQ_REFCOUNT_ASSERT, IRQ_REFCOUNT_DEASSERT, IRQ_REFCOUNT_SATURATED, IRQ_REFCOUNT_UNDERFLOW, applyIrqRefCountChange } from "../io/irq_refcount.ts";
import type { IrqSink } from "../io/device_manager.ts";
import type { SerialOutputSink } from "../io/devices/uart16550.ts";

const IS_DEV = (import.meta as { env?: { DEV?: boolean } }).env?.DEV === true;

export interface IoWorkerInitOptions {
  requestRing: SharedArrayBuffer;
  responseRing: SharedArrayBuffer;
  tickIntervalMs?: number;
  devices?: string[];
  // Optional worker-local stop flag (Int32Array length >= 1). Primarily used by Node tests to
  // request a graceful shutdown without relying on `Worker.terminate()` semantics.
  stopSignal?: SharedArrayBuffer;
}

export function runIoWorkerServer(opts: IoWorkerInitOptions): void {
  const reqRing = SharedRingBuffer.from(opts.requestRing);
  const respRing = SharedRingBuffer.from(opts.responseRing);
  if (reqRing.stride !== IO_MESSAGE_STRIDE_U32 || respRing.stride !== IO_MESSAGE_STRIDE_U32) {
    throw new Error("IO rings have unexpected stride; did you allocate with IO_MESSAGE_STRIDE_U32?");
  }

  const irqTx = new Uint32Array(IO_MESSAGE_STRIDE_U32);
  const serialTx = new Uint32Array(IO_MESSAGE_STRIDE_U32);
  const irqRefCounts = new Uint16Array(256);
  const irqWarnedUnderflow = new Uint8Array(256);
  const irqWarnedSaturated = new Uint8Array(256);
  const irqSink: IrqSink = {
    raiseIrq: (irq) => {
      // IRQs are transported as line level transitions (assert/deassert). Edge-triggered sources
      // are represented as explicit pulses (raise then lower). See `docs/irq-semantics.md`.
      const idx = irq & 0xff;
      const flags = applyIrqRefCountChange(irqRefCounts, idx, true);
      if (flags & IRQ_REFCOUNT_ASSERT) {
        writeIoMessage(irqTx, { type: IO_OP_IRQ_RAISE, id: 0, addrLo: idx, addrHi: 0, size: 0, value: 0 });
        respRing.pushBlocking(irqTx);
      }
      if (IS_DEV && (flags & IRQ_REFCOUNT_SATURATED) && irqWarnedSaturated[idx] === 0) {
        irqWarnedSaturated[idx] = 1;
        console.warn(`[io_worker_runtime] IRQ${idx} refcount saturated at 0xffff (raiseIrq without matching lowerIrq?)`);
      }
    },
    lowerIrq: (irq) => {
      const idx = irq & 0xff;
      const flags = applyIrqRefCountChange(irqRefCounts, idx, false);
      if (flags & IRQ_REFCOUNT_DEASSERT) {
        writeIoMessage(irqTx, { type: IO_OP_IRQ_LOWER, id: 0, addrLo: idx, addrHi: 0, size: 0, value: 0 });
        respRing.pushBlocking(irqTx);
      }
      if (IS_DEV && (flags & IRQ_REFCOUNT_UNDERFLOW) && irqWarnedUnderflow[idx] === 0) {
        irqWarnedUnderflow[idx] = 1;
        console.warn(`[io_worker_runtime] IRQ${idx} refcount underflow (lowerIrq while already deasserted)`);
      }
    },
  };

  const sysCtrlTx = new Uint32Array(IO_MESSAGE_STRIDE_U32);
  const systemControl = {
    setA20: (enabled: boolean) => {
      writeIoMessage(sysCtrlTx, { type: IO_OP_A20_SET, id: 0, addrLo: 0, addrHi: 0, size: 0, value: enabled ? 1 : 0 });
      respRing.pushBlocking(sysCtrlTx);
    },
    requestReset: () => {
      writeIoMessage(sysCtrlTx, { type: IO_OP_RESET_REQUEST, id: 0, addrLo: 0, addrHi: 0, size: 0, value: 1 });
      respRing.pushBlocking(sysCtrlTx);
    },
  };

  const serialSink: SerialOutputSink = {
    write: (port, data) => {
      for (let i = 0; i < data.byteLength; i += 4) {
        const chunk = data.subarray(i, i + 4);
        let packed = 0;
        for (let j = 0; j < chunk.byteLength; j++) {
          packed |= (chunk[j]! & 0xff) << (j * 8);
        }

        writeIoMessage(serialTx, {
          type: IO_OP_SERIAL_OUT,
          id: 0,
          addrLo: port & 0xffff,
          addrHi: 0,
          size: chunk.byteLength & 0xff,
          value: packed >>> 0,
        });
        respRing.pushBlocking(serialTx);
      }
    },
  };

  const devices = opts.devices ?? ["i8042"];
  const mgr = new DeviceManager(irqSink);

  if (devices.includes("i8042")) {
    const i8042 = new I8042Controller(mgr.irqSink, { systemControl });
    // Avoid stealing unrelated ports (e.g. 0x61 is PPI); map only the canonical ports.
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

  const server = new IoServer(reqRing, respRing, mgr, { tickIntervalMs: opts.tickIntervalMs });
  const stopSignal = opts.stopSignal ? new Int32Array(opts.stopSignal) : undefined;
  server.run(stopSignal);
}
