import { PERF_RECORD_SIZE_BYTES, WorkerKind } from "./record.js";
import { createSpscRingBufferSharedArrayBuffer } from "./ring_buffer.js";

export const PERF_FRAME_HEADER_FRAME_ID_INDEX = 0;
export const PERF_FRAME_HEADER_T_US_INDEX = 1;
export const PERF_FRAME_HEADER_I32_LEN = 2;

export function nowEpochMs() {
  // `performance.timeOrigin + performance.now()` is available in both Window and Worker
  // contexts and produces an epoch-ish timestamp.
  return performance.timeOrigin + performance.now();
}

export function createPerfChannel({
  capacity = 2048,
  workerKinds = [WorkerKind.Main, WorkerKind.CPU, WorkerKind.GPU, WorkerKind.IO, WorkerKind.JIT],
} = {}) {
  const runStartEpochMs = nowEpochMs();

  const frameHeader = new SharedArrayBuffer(PERF_FRAME_HEADER_I32_LEN * Int32Array.BYTES_PER_ELEMENT);

  const buffers = {};
  for (const kind of workerKinds) {
    buffers[kind] = createSpscRingBufferSharedArrayBuffer({
      capacity,
      recordSize: PERF_RECORD_SIZE_BYTES,
    });
  }

  return {
    schemaVersion: 1,
    runStartEpochMs,
    capacity,
    recordSize: PERF_RECORD_SIZE_BYTES,
    frameHeader,
    buffers,
  };
}
