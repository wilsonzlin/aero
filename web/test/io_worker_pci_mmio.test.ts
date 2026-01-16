import test from "node:test";
import assert from "node:assert/strict";
import { Worker } from "node:worker_threads";
import { once } from "node:events";

import { SharedRingBuffer } from "../src/io/ipc/ring_buffer.ts";
import { IO_MESSAGE_STRIDE_U32 } from "../src/io/ipc/io_protocol.ts";
import { PCI_MMIO_BASE } from "../src/arch/guest_phys.ts";

async function stopWorker(worker: Worker, timeoutMs = 2000): Promise<void> {
  worker.unref();
  const exited = await Promise.race([
    once(worker, "exit").then(() => true),
    new Promise<boolean>((resolve) => setTimeout(() => resolve(false), timeoutMs)),
  ]);
  if (exited) return;
  await Promise.race([
    worker.terminate(),
    new Promise<void>((resolve) => setTimeout(resolve, timeoutMs)),
  ]);
}

test("I/O worker: PCI config (0xCF8/0xCFC) + BAR-backed MMIO dispatch", async () => {
  const req = SharedRingBuffer.create({ capacity: 128, stride: IO_MESSAGE_STRIDE_U32 });
  const resp = SharedRingBuffer.create({ capacity: 128, stride: IO_MESSAGE_STRIDE_U32 });
  const stopSignal = new SharedArrayBuffer(4);
  const stop = new Int32Array(stopSignal);

  const ioWorker = new Worker(new URL("../src/workers/io_worker_node.ts", import.meta.url), {
    type: "module",
    workerData: {
      requestRing: req.sab,
      responseRing: resp.sab,
      stopSignal,
      devices: ["pci_test"],
      tickIntervalMs: 1,
    },
    execArgv: ["--experimental-strip-types"],
  });

  const cpuWorker = new Worker(new URL("./workers/cpu_sequence_worker.ts", import.meta.url), {
    type: "module",
    workerData: {
      scenario: "pci_test",
      requestRing: req.sab,
      responseRing: resp.sab,
    },
    execArgv: ["--experimental-strip-types"],
  });

  try {
    const [result] = (await once(cpuWorker, "message")) as [
      {
        ok: boolean;
        idDword: number;
        ssidDword: number;
        ssidDwordAfter: number;
        irqLineBefore: number;
        irqLineAfter: number;
        irqPinBefore: number;
        irqPinAfter: number;
        bar0: number;
        bar1Before: number;
        bar1After: number;
        mmioReadback: number;
      },
    ];

    assert.equal(result.ok, true);
    assert.equal(result.idDword >>> 0, 0x5678_1234);
    assert.equal(result.ssidDword >>> 0, 0xef01_abcd);
    assert.equal(result.ssidDwordAfter >>> 0, 0xef01_abcd);
    assert.equal(result.irqLineBefore >>> 0, 0x0b);
    assert.equal(result.irqLineAfter >>> 0, 0x0c);
    assert.equal(result.irqPinBefore >>> 0, 0x02);
    assert.equal(result.irqPinAfter >>> 0, 0x02);
    assert.equal(result.bar0 >>> 0, PCI_MMIO_BASE);
    assert.equal(result.bar1Before >>> 0, 0x0000_0000);
    assert.equal(result.bar1After >>> 0, 0x0000_0000);
    assert.equal(result.mmioReadback >>> 0, 0x1234_5678);
  } finally {
    Atomics.store(stop, 0, 1);
    await Promise.allSettled([stopWorker(cpuWorker), stopWorker(ioWorker)]);
  }
});
