import test from "node:test";
import assert from "node:assert/strict";
import { Worker } from "node:worker_threads";
import { once } from "node:events";

import { SharedRingBuffer } from "../src/io/ipc/ring_buffer.ts";
import { IO_MESSAGE_STRIDE_U32 } from "../src/io/ipc/io_protocol.ts";
import { WORKER_EXEC_ARGV } from "./_helpers/worker_exec_argv.ts";

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

test("I/O worker: i8042 port I/O + IRQ signalling", async () => {
  const req = SharedRingBuffer.create({ capacity: 64, stride: IO_MESSAGE_STRIDE_U32 });
  const resp = SharedRingBuffer.create({ capacity: 64, stride: IO_MESSAGE_STRIDE_U32 });
  const stopSignal = new SharedArrayBuffer(4);
  const stop = new Int32Array(stopSignal);

  const ioWorker = new Worker(new URL("../src/workers/io_worker_node.ts", import.meta.url), {
    type: "module",
    workerData: {
      requestRing: req.sab,
      responseRing: resp.sab,
      stopSignal,
      devices: ["i8042"],
      tickIntervalMs: 1,
    },
    execArgv: WORKER_EXEC_ARGV,
  });

  const cpuWorker = new Worker(new URL("./workers/cpu_sequence_worker.ts", import.meta.url), {
    type: "module",
    workerData: {
      scenario: "i8042",
      requestRing: req.sab,
      responseRing: resp.sab,
    },
    execArgv: WORKER_EXEC_ARGV,
  });

  try {
    const [result] = (await once(cpuWorker, "message")) as [
      {
        ok: boolean;
        statusBefore: number;
        statusMid: number;
        statusAfter: number;
        bytes: number[];
        irqEvents: Array<{ irq: number; level: boolean }>;
      },
    ];

    assert.equal(result.ok, true);
    assert.deepEqual(result.bytes, [0xfa, 0xaa]);
    assert.equal(result.statusBefore & 0x01, 0x01, "OBF should be set after keyboard reset reply queued");
    assert.equal(result.statusMid & 0x01, 0x01, "OBF should remain set while bytes remain in queue");
    assert.equal(result.statusAfter & 0x01, 0x00, "OBF should clear after draining queue");

    assert.deepEqual(
      result.irqEvents,
      [
        { irq: 1, level: true },
        { irq: 1, level: false },
        { irq: 1, level: true },
        { irq: 1, level: false },
      ],
      "expected edge-style IRQ1 pulses (one per output byte)"
    );
  } finally {
    Atomics.store(stop, 0, 1);
    await Promise.allSettled([stopWorker(cpuWorker), stopWorker(ioWorker)]);
  }
});

