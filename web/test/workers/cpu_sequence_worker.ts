import { parentPort, workerData } from "node:worker_threads";
import { IoClient } from "../../src/io/ipc/io_client.ts";
import { SharedRingBuffer } from "../../src/io/ipc/ring_buffer.ts";

const req = SharedRingBuffer.from(workerData.requestRing as SharedArrayBuffer);
const resp = SharedRingBuffer.from(workerData.responseRing as SharedArrayBuffer);

const irqEvents: Array<{ irq: number; level: boolean }> = [];
const a20Events: boolean[] = [];
let resetRequests = 0;
const serialBytes: number[] = [];
const io = new IoClient(req, resp, {
  onIrq: (irq, level) => {
    irqEvents.push({ irq, level });
  },
  onA20: (enabled) => {
    a20Events.push(enabled);
  },
  onReset: () => {
    resetRequests++;
  },
  onSerialOutput: (_port, data) => {
    for (const b of data) serialBytes.push(b);
  },
});

try {
  if (workerData.scenario === "i8042") {
    // Enable keyboard interrupts (bit0 in command byte).
    io.portWrite(0x64, 1, 0x60);
    io.portWrite(0x60, 1, 0x01);

    // Keyboard reset triggers a 0xFA ACK + 0xAA self-test response.
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
  } else if (workerData.scenario === "i8042_output_port") {
    // Write output port: enable A20 (bit1) while keeping reset deasserted (bit0=1).
    io.portWrite(0x64, 1, 0xd1);
    io.portWrite(0x60, 1, 0x03);

    // Disable A20 again.
    io.portWrite(0x64, 1, 0xd1);
    io.portWrite(0x60, 1, 0x01);

    // Assert reset line (bit0 active-low): transition 1 -> 0 triggers reset request.
    io.portWrite(0x64, 1, 0xd1);
    io.portWrite(0x60, 1, 0x00);

    // Read output port returns the last written value.
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
  } else if (workerData.scenario === "pci_test") {
    // Read vendor/device id dword.
    io.portWrite(0x0cf8, 4, 0x8000_0000);
    const idDword = io.portRead(0x0cfc, 4);

    // Read BAR0 (offset 0x10).
    io.portWrite(0x0cf8, 4, 0x8000_0010);
    const bar0 = io.portRead(0x0cfc, 4);

    // Enable memory space decoding (PCI command bit1) so the BAR-backed MMIO region is active.
    io.portWrite(0x0cf8, 4, 0x8000_0004);
    io.portWrite(0x0cfc, 2, 0x0002);

    const base = BigInt(bar0 >>> 0);
    io.mmioWrite(base + 0n, 4, 0x1234_5678);
    const mmioReadback = io.mmioRead(base + 0n, 4);

    parentPort!.postMessage({ ok: true, idDword, bar0, mmioReadback, irqEvents });
  } else if (workerData.scenario === "uart16550") {
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
  } else {
    parentPort!.postMessage({ ok: false, error: `unknown scenario: ${workerData.scenario}` });
  }
} catch (err) {
  parentPort!.postMessage({ ok: false, error: String(err), irqEvents });
}
