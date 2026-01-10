export type PerfTimeBreakdownMs = {
  cpu?: number;
  gpu?: number;
  io?: number;
  jit?: number;
};

export type PerfCaptureState = {
  active: boolean;
  durationMs: number;
  droppedRecords: number;
  records: number;
};

export type PerfHudSnapshot = {
  nowMs: number;

  fpsAvg?: number;
  fps1Low?: number;
  frameTimeAvgMs?: number;
  frameTimeP95Ms?: number;

  mipsAvg?: number;

  lastFrameTimeMs?: number;
  lastMips?: number;

  breakdownAvgMs?: PerfTimeBreakdownMs;

  drawCallsPerFrame?: number;
  ioBytesPerSec?: number;

  hostJsHeapUsedBytes?: number;
  hostJsHeapTotalBytes?: number;

  guestRamBytes?: number;

  capture: PerfCaptureState;
};

export interface PerfApi {
  getHudSnapshot(out: PerfHudSnapshot): PerfHudSnapshot;
  setHudActive(active: boolean): void;

  captureStart(): void;
  captureStop(): void;
  captureReset(): void;
  export(): unknown;
}

