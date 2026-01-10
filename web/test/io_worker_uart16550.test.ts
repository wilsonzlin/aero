import test from "node:test";
import assert from "node:assert/strict";
import { Worker } from "node:worker_threads";
import { once } from "node:events";

import { SharedRingBuffer } from "../src/io/ipc/ring_buffer.ts";
import { IO_MESSAGE_STRIDE_U32 } from "../src/io/ipc/io_protocol.ts";

test("I/O worker: 16550 UART emits serial output bytes", async () => {
  const req = SharedRingBuffer.create({ capacity: 128, stride: IO_MESSAGE_STRIDE_U32 });
  const resp = SharedRingBuffer.create({ capacity: 128, stride: IO_MESSAGE_STRIDE_U32 });

  const ioWorker = new Worker(new URL("../src/workers/io_worker_node.ts", import.meta.url), {
    type: "module",
    workerData: {
      requestRing: req.sab,
      responseRing: resp.sab,
      devices: ["uart16550"],
      tickIntervalMs: 1,
    },
    execArgv: ["--experimental-strip-types"],
  });

  const cpuWorker = new Worker(new URL("./workers/cpu_sequence_worker.ts", import.meta.url), {
    type: "module",
    workerData: {
      scenario: "uart16550",
      requestRing: req.sab,
      responseRing: resp.sab,
    },
    execArgv: ["--experimental-strip-types"],
  });

  const [result] = (await once(cpuWorker, "message")) as [
    { ok: boolean; lsrBefore: number; lsrAfter: number; serialBytes: number[] },
  ];

  assert.equal(result.ok, true);
  assert.equal(result.lsrBefore & 0x60, 0x60);
  assert.equal(result.lsrAfter & 0x60, 0x60);
  assert.deepEqual(result.serialBytes, [0x48, 0x69]);

  await cpuWorker.terminate();
  await ioWorker.terminate();
});

