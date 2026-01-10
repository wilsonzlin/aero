import { SpscRingBuffer } from "./ring_buffer.js";
import { encodeFrameSampleRecord, makeEncodedFrameSample, PERF_RECORD_SIZE_BYTES } from "./record.js";
import { nowEpochMs } from "./shared.js";

export class PerfWriter {
  constructor(sharedArrayBuffer, { workerKind, runStartEpochMs, enabled = true } = {}) {
    if (workerKind == null) {
      throw new Error(`PerfWriter requires workerKind`);
    }
    if (runStartEpochMs == null) {
      throw new Error(`PerfWriter requires runStartEpochMs`);
    }
    this.ring = new SpscRingBuffer(sharedArrayBuffer, { expectedRecordSize: PERF_RECORD_SIZE_BYTES });
    this.workerKind = workerKind >>> 0;
    this.runStartEpochMs = runStartEpochMs;
    this.enabled = !!enabled;
  }

  setEnabled(enabled) {
    this.enabled = !!enabled;
  }

  /**
   * Emit a merged-friendly per-frame sample.
   *
   * @param {number} frameId u32
   * @param {{
   *   durations?: { frame_ms?: number, cpu_ms?: number, gpu_ms?: number, io_ms?: number, jit_ms?: number },
   *   counters?: { instructions?: bigint | number, memory_bytes?: bigint | number, draw_calls?: number, io_read_bytes?: number, io_write_bytes?: number },
   *   now_epoch_ms?: number,
   * }} sample
   */
  frameSample(frameId, sample = {}) {
    if (!this.enabled) {
      return false;
    }
    const nowMs = sample.now_epoch_ms ?? nowEpochMs();
    const tUs = Math.max(0, Math.min(0xffff_ffff, Math.round((nowMs - this.runStartEpochMs) * 1000))) >>> 0;

    const durations = sample.durations ?? {};
    const counters = sample.counters ?? {};

    const encoded = makeEncodedFrameSample({
      workerKind: this.workerKind,
      frameId,
      tUs,
      frameMs: durations.frame_ms ?? 0,
      cpuMs: durations.cpu_ms ?? 0,
      gpuMs: durations.gpu_ms ?? 0,
      ioMs: durations.io_ms ?? 0,
      jitMs: durations.jit_ms ?? 0,
      instructions: counters.instructions ?? 0n,
      memoryBytes: counters.memory_bytes ?? 0n,
      drawCalls: counters.draw_calls ?? 0,
      ioReadBytes: counters.io_read_bytes ?? 0,
      ioWriteBytes: counters.io_write_bytes ?? 0,
    });

    return this.ring.tryWriteRecord((view, byteOffset) => {
      encodeFrameSampleRecord(view, byteOffset, encoded);
    });
  }

  // Snake_case alias for callers matching the PF task spec.
  frame_sample(frameId, sample = {}) {
    return this.frameSample(frameId, sample);
  }
}
