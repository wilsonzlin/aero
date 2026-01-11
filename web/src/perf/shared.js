import { PERF_RECORD_SIZE_BYTES, WorkerKind } from "./record.js";
import { createSpscRingBufferSharedArrayBuffer } from "./ring_buffer.js";

// Shared perf frame header (Int32Array over SharedArrayBuffer) used to coordinate
// per-frame samples across the main thread + workers.
//
// Layout (all values are u32 stored in Int32Array slots):
// - FRAME_ID: monotonically increasing frame counter written by PerfSession (0 while inactive)
// - T_US: `performance.now()` timestamp in microseconds since run start
// - ENABLED: 0/1 gate set by PerfSession so workers can skip perf writes when HUD/capture is inactive
export const PERF_FRAME_HEADER_FRAME_ID_INDEX = 0;
export const PERF_FRAME_HEADER_T_US_INDEX = 1;
export const PERF_FRAME_HEADER_ENABLED_INDEX = 2;
export const PERF_FRAME_HEADER_I32_LEN = 3;

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
