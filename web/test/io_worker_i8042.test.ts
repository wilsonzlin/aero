import test from "node:test";
import assert from "node:assert/strict";
import { Worker } from "node:worker_threads";
import { once } from "node:events";

import { SharedRingBuffer } from "../src/io/ipc/ring_buffer.ts";
import { IO_MESSAGE_STRIDE_U32 } from "../src/io/ipc/io_protocol.ts";

test("I/O worker: i8042 port I/O + IRQ signalling", async () => {
  const req = SharedRingBuffer.create({ capacity: 64, stride: IO_MESSAGE_STRIDE_U32 });
  const resp = SharedRingBuffer.create({ capacity: 64, stride: IO_MESSAGE_STRIDE_U32 });

  const ioWorker = new Worker(new URL("../src/workers/io_worker_node.ts", import.meta.url), {
    type: "module",
    workerData: {
      requestRing: req.sab,
      responseRing: resp.sab,
      devices: ["i8042"],
      tickIntervalMs: 1,
    },
    execArgv: ["--experimental-strip-types"],
  });

  const cpuWorker = new Worker(new URL("./workers/cpu_sequence_worker.ts", import.meta.url), {
    type: "module",
    workerData: {
      scenario: "i8042",
      requestRing: req.sab,
      responseRing: resp.sab,
    },
    execArgv: ["--experimental-strip-types"],
  });

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
    ],
    "expected level-based IRQ1 assert/deassert"
  );

  await cpuWorker.terminate();
  await ioWorker.terminate();
});

