import { StatusIndex } from "./shared_layout";

export type IoInputTelemetrySnapshot = {
  /**
   * Total input batches received by the I/O worker (includes batches that were queued
   * while snapshot-paused, and batches that were dropped).
   */
  batchesReceived: number;
  /**
   * Total input batches processed by the I/O worker input pipeline.
   *
   * Note: this is currently backed by `StatusIndex.IoInputBatchCounter` for
   * backwards compatibility.
   */
  batchesProcessed: number;
  /**
   * Total input batches dropped by the I/O worker (e.g. while snapshot-paused when
   * the bounded queue is full).
   */
  batchesDropped: number;
  /**
   * Total backend switches for keyboard input (ps2↔usb↔virtio).
   */
  keyboardBackendSwitches: number;
  /**
   * Total backend switches for mouse input (ps2↔usb↔virtio).
   */
  mouseBackendSwitches: number;
};

export function readIoInputTelemetry(status: Int32Array): IoInputTelemetrySnapshot {
  return {
    batchesReceived: Atomics.load(status, StatusIndex.IoInputBatchReceivedCounter) >>> 0,
    batchesProcessed: Atomics.load(status, StatusIndex.IoInputBatchCounter) >>> 0,
    batchesDropped: Atomics.load(status, StatusIndex.IoInputBatchDropCounter) >>> 0,
    keyboardBackendSwitches: Atomics.load(status, StatusIndex.IoKeyboardBackendSwitchCounter) >>> 0,
    mouseBackendSwitches: Atomics.load(status, StatusIndex.IoMouseBackendSwitchCounter) >>> 0,
  };
}

