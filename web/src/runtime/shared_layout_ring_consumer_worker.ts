import { parentPort, workerData } from "node:worker_threads";

import { RingBuffer } from "../ipc/ring_buffer.ts";

type WorkerData = {
  sab: SharedArrayBuffer;
  offsetBytes: number;
  count: number;
};

const { sab, offsetBytes, count } = workerData as WorkerData;

const ring = new RingBuffer(sab, offsetBytes);

const received: number[] = [];

function run(): void {
  while (received.length < count) {
    const msg = ring.tryPop();
    if (!msg) {
      ring.waitForData(1000);
      continue;
    }
    if (msg.byteLength !== 4) continue;
    const value = new DataView(msg.buffer, msg.byteOffset, msg.byteLength).getUint32(0, true);
    received.push(value);
  }

  parentPort?.postMessage(received);
}

run();

