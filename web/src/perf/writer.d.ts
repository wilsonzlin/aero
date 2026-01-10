export type FrameSampleDurations = {
  frame_ms?: number;
  cpu_ms?: number;
  gpu_ms?: number;
  io_ms?: number;
  jit_ms?: number;
};

export type FrameSampleCounters = {
  instructions?: bigint | number;
  memory_bytes?: bigint | number;
  draw_calls?: number;
  io_read_bytes?: number;
  io_write_bytes?: number;
};

export type FrameSample = {
  durations?: FrameSampleDurations;
  counters?: FrameSampleCounters;
  now_epoch_ms?: number;
};

export class PerfWriter {
  constructor(
    sharedArrayBuffer: SharedArrayBuffer,
    options: { workerKind: number; runStartEpochMs: number; enabled?: boolean },
  );

  setEnabled(enabled: boolean): void;
  frameSample(frameId: number, sample?: FrameSample): boolean;
  frame_sample(frameId: number, sample?: FrameSample): boolean;
}

