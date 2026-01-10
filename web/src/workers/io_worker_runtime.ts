import { DeviceManager } from "../io/device_manager.ts";
import { I8042Controller } from "../io/devices/i8042.ts";
import { PciTestDevice } from "../io/devices/pci_test_device.ts";
import { IoServer } from "../io/ipc/io_server.ts";
import { SharedRingBuffer } from "../io/ipc/ring_buffer.ts";
import {
  IO_MESSAGE_STRIDE_U32,
  IO_OP_A20_SET,
  IO_OP_IRQ_LOWER,
  IO_OP_IRQ_RAISE,
  IO_OP_RESET_REQUEST,
  writeIoMessage,
} from "../io/ipc/io_protocol.ts";
import type { IrqSink } from "../io/device_manager.ts";

export interface IoWorkerInitOptions {
  requestRing: SharedArrayBuffer;
  responseRing: SharedArrayBuffer;
  tickIntervalMs?: number;
  devices?: string[];
}

export function runIoWorkerServer(opts: IoWorkerInitOptions): never {
  const reqRing = SharedRingBuffer.from(opts.requestRing);
  const respRing = SharedRingBuffer.from(opts.responseRing);
  if (reqRing.stride !== IO_MESSAGE_STRIDE_U32 || respRing.stride !== IO_MESSAGE_STRIDE_U32) {
    throw new Error("IO rings have unexpected stride; did you allocate with IO_MESSAGE_STRIDE_U32?");
  }

  const irqTx = new Uint32Array(IO_MESSAGE_STRIDE_U32);
  const irqSink: IrqSink = {
    raiseIrq: (irq) => {
      writeIoMessage(irqTx, { type: IO_OP_IRQ_RAISE, id: 0, addrLo: irq & 0xff, addrHi: 0, size: 0, value: 0 });
      respRing.pushBlocking(irqTx);
    },
    lowerIrq: (irq) => {
      writeIoMessage(irqTx, { type: IO_OP_IRQ_LOWER, id: 0, addrLo: irq & 0xff, addrHi: 0, size: 0, value: 0 });
      respRing.pushBlocking(irqTx);
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

  const server = new IoServer(reqRing, respRing, mgr, { tickIntervalMs: opts.tickIntervalMs });
  return server.run();
}
