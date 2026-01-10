import { parentPort, workerData } from "node:worker_threads";

import { RingBuffer } from "../../src/ipc/ring_buffer.ts";
import { AeroIpcIoClient } from "../../src/io/ipc/aero_ipc_io.ts";

const { sab, cmdOffset, evtOffset, scenario } = workerData as {
  sab: SharedArrayBuffer;
  cmdOffset: number;
  evtOffset: number;
  scenario: "i8042" | "pci_test";
};

const cmdQ = new RingBuffer(sab, cmdOffset);
const evtQ = new RingBuffer(sab, evtOffset);

const irqEvents: Array<{ irq: number; level: boolean }> = [];
const io = new AeroIpcIoClient(cmdQ, evtQ, {
  onIrq: (irq, level) => irqEvents.push({ irq, level }),
});

try {
  if (scenario === "i8042") {
    io.portWrite(0x64, 1, 0x60);
    io.portWrite(0x60, 1, 0x01);
    io.portWrite(0x60, 1, 0xff);

    const statusBefore = io.portRead(0x64, 1);
    const b0 = io.portRead(0x60, 1);
    const statusMid = io.portRead(0x64, 1);
    const b1 = io.portRead(0x60, 1);
    const statusAfter = io.portRead(0x64, 1);

    parentPort!.postMessage({
      ok: true,
      statusBefore,
      statusMid,
      statusAfter,
      bytes: [b0, b1],
      irqEvents,
    });
  } else if (scenario === "pci_test") {
    io.portWrite(0x0cf8, 4, 0x8000_0000);
    const idDword = io.portRead(0x0cfc, 4);

    io.portWrite(0x0cf8, 4, 0x8000_0010);
    const bar0 = io.portRead(0x0cfc, 4);

    const base = BigInt(bar0 >>> 0);
    io.mmioWrite(base + 0n, 4, 0x1234_5678);
    const mmioReadback = io.mmioRead(base + 0n, 4);

    parentPort!.postMessage({ ok: true, idDword, bar0, mmioReadback, irqEvents });
  } else {
    parentPort!.postMessage({ ok: false, error: `unknown scenario: ${scenario}`, irqEvents });
  }
} catch (err) {
  parentPort!.postMessage({ ok: false, error: String(err), irqEvents });
}

