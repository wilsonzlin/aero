import { parentPort, workerData } from "node:worker_threads";

import { RingBuffer } from "../../src/ipc/ring_buffer.ts";
import { AeroIpcIoClient } from "../../src/io/ipc/aero_ipc_io.ts";

const { sab, cmdOffset, evtOffset, scenario } = workerData as {
  sab: SharedArrayBuffer;
  cmdOffset: number;
  evtOffset: number;
  scenario: "i8042" | "i8042_output_port" | "pci_test" | "uart16550";
};

const cmdQ = new RingBuffer(sab, cmdOffset);
const evtQ = new RingBuffer(sab, evtOffset);

const irqEvents: Array<{ irq: number; level: boolean }> = [];
const a20Events: boolean[] = [];
let resetRequests = 0;
const serialBytes: number[] = [];
const io = new AeroIpcIoClient(cmdQ, evtQ, {
  onIrq: (irq, level) => irqEvents.push({ irq, level }),
  onA20: (enabled) => a20Events.push(enabled),
  onReset: () => {
    resetRequests += 1;
  },
  onSerialOutput: (_port, data) => {
    for (const b of data) serialBytes.push(b);
  },
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
      a20Events,
      resetRequests,
    });
    parentPort!.close();
    process.exit(0);
  } else if (scenario === "i8042_output_port") {
    io.portWrite(0x64, 1, 0xd1);
    io.portWrite(0x60, 1, 0x03);

    io.portWrite(0x64, 1, 0xd1);
    io.portWrite(0x60, 1, 0x01);

    io.portWrite(0x64, 1, 0xd1);
    io.portWrite(0x60, 1, 0x00);

    io.portWrite(0x64, 1, 0xd1);
    io.portWrite(0x60, 1, 0xa9);
    io.portWrite(0x64, 1, 0xd0);
    const outPort = io.portRead(0x60, 1);

    parentPort!.postMessage({
      ok: true,
      outPort,
      a20Events,
      resetRequests,
      irqEvents,
    });
    parentPort!.close();
    process.exit(0);
  } else if (scenario === "pci_test") {
    io.portWrite(0x0cf8, 4, 0x8000_0000);
    const idDword = io.portRead(0x0cfc, 4);

    io.portWrite(0x0cf8, 4, 0x8000_0010);
    const bar0 = io.portRead(0x0cfc, 4);

    // Enable memory space decoding (PCI command bit1) so the BAR-backed MMIO region is active.
    io.portWrite(0x0cf8, 4, 0x8000_0004);
    io.portWrite(0x0cfc, 2, 0x0002);

    const base = BigInt(bar0 >>> 0);
    io.mmioWrite(base + 0n, 4, 0x1234_5678);
    const mmioReadback = io.mmioRead(base + 0n, 4);

    parentPort!.postMessage({ ok: true, idDword, bar0, mmioReadback, irqEvents });
    parentPort!.close();
    process.exit(0);
  } else if (scenario === "uart16550") {
    const lsrBefore = io.portRead(0x3f8 + 5, 1);
    io.portWrite(0x3f8, 1, "H".charCodeAt(0));
    io.portWrite(0x3f8, 1, "i".charCodeAt(0));
    const lsrAfter = io.portRead(0x3f8 + 5, 1);

    parentPort!.postMessage({
      ok: true,
      lsrBefore,
      lsrAfter,
      serialBytes,
      irqEvents,
    });
    parentPort!.close();
    process.exit(0);
  } else {
    parentPort!.postMessage({ ok: false, error: `unknown scenario: ${scenario}`, irqEvents });
    parentPort!.close();
    process.exit(1);
  }
} catch (err) {
  parentPort!.postMessage({ ok: false, error: String(err), irqEvents });
  parentPort!.close();
  process.exit(1);
}
