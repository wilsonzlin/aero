import { parentPort, workerData } from "node:worker_threads";

import { RingBuffer } from "./ring_buffer.ts";

type WorkerData = {
  sab: SharedArrayBuffer;
  byteOffset: number;
  byteLength: number;
  count: number;
};

const { sab, byteOffset, byteLength, count } = workerData as WorkerData;

const ring = new RingBuffer(sab, byteOffset, byteLength);

const received: number[] = [];

async function run(): Promise<void> {
  while (received.length < count) {
    const msg = ring.pop();
    if (!msg) {
      await ring.waitForData(1000);
      continue;
    }
    if (msg.byteLength !== 4) continue;
    const value = new DataView(msg.buffer, msg.byteOffset, msg.byteLength).getUint32(0, true);
    received.push(value);
  }

  parentPort?.postMessage(received);
}

void run();
