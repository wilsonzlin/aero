import test from "node:test";
import assert from "node:assert/strict";
import { Worker } from "node:worker_threads";
import { once } from "node:events";

import { SharedRingBuffer } from "../src/io/ipc/ring_buffer.ts";
import { WORKER_EXEC_ARGV } from "./_helpers/worker_exec_argv.ts";

test("SharedRingBuffer: wraparound + full/empty behavior", () => {
  const ring = SharedRingBuffer.create({ capacity: 4, stride: 1 });
  const out = new Uint32Array(1);

  assert.equal(ring.push([1]), true);
  assert.equal(ring.push([2]), true);
  assert.equal(ring.push([3]), true);
  assert.equal(ring.push([4]), false, "ring should be full (capacity-1 usable)");

  assert.equal(ring.popInto(out), true);
  assert.equal(out[0], 1);

  assert.equal(ring.push([4]), true, "should wrap and accept new element after pop");

  assert.equal(ring.popInto(out), true);
  assert.equal(out[0], 2);
  assert.equal(ring.popInto(out), true);
  assert.equal(out[0], 3);
  assert.equal(ring.popInto(out), true);
  assert.equal(out[0], 4);
  assert.equal(ring.popInto(out), false, "ring should be empty");
});

test("SharedRingBuffer: Atomics.wait/notify popBlockingInto()", async () => {
  const ring = SharedRingBuffer.create({ capacity: 8, stride: 1 });

  const worker = new Worker(new URL("./workers/ring_pop_worker.ts", import.meta.url), {
    type: "module",
    workerData: { ring: ring.sab },
    execArgv: WORKER_EXEC_ARGV,
  });

  try {
    // Let the worker block on Atomics.wait() first.
    await new Promise((r) => setTimeout(r, 25));
    assert.equal(ring.push([0xdead_beef]), true);

    const [msg] = (await once(worker, "message")) as [{ ok: boolean; value: number }];
    assert.equal(msg.ok, true);
    assert.equal(msg.value >>> 0, 0xdead_beef);
  } finally {
    worker.unref();
    const done = worker.terminate();
    await Promise.race([done, new Promise<void>((resolve) => setTimeout(resolve, 2000))]);
  }
});

