import { parentPort, workerData } from "node:worker_threads";
import { SharedRingBuffer } from "../../src/io/ipc/ring_buffer.ts";

const ring = SharedRingBuffer.from(workerData.ring as SharedArrayBuffer);
const out = new Uint32Array(ring.stride);

const ok = ring.popBlockingInto(out, 2000);
parentPort!.postMessage({ ok, value: out[0] ?? 0 });

