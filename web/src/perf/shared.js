import { PERF_RECORD_SIZE_BYTES, WorkerKind } from "./record.js";
import { createSpscRingBufferSharedArrayBuffer } from "./ring_buffer.js";

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
    buffers,
  };
}

