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

export type GraphicsSampleCounters = {
  render_passes?: number;
  pipeline_switches?: number;
  bind_group_changes?: number;
  upload_bytes?: bigint | number;
};

export type GraphicsSampleDurations = {
  cpu_translate_ms?: number;
  cpu_encode_ms?: number;
  gpu_time_ms?: number | null;
};

export type GraphicsSampleGpuTiming = {
  supported?: boolean;
  enabled?: boolean;
};

export type GraphicsSample = {
  counters?: GraphicsSampleCounters;
  durations?: GraphicsSampleDurations;
  gpu_timing?: GraphicsSampleGpuTiming;
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

  graphicsSample(frameId: number, sample?: GraphicsSample): boolean;
  graphics_sample(frameId: number, sample?: GraphicsSample): boolean;
}
