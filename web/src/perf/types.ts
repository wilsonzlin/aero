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

export type PerfJitTierTotals = {
  blocksCompiled: number;
  compileMs: number;
};

export type PerfJitTier2PassesMs = {
  constFold: number;
  dce: number;
  regalloc: number;
};

export type PerfJitTier2Totals = PerfJitTierTotals & {
  passesMs: PerfJitTier2PassesMs;
};

export type PerfJitCacheTotals = {
  lookupHit: number;
  lookupMiss: number;
  capacityBytes: number;
  usedBytes: number;
};

export type PerfJitDeoptTotals = {
  count: number;
  guardFail: number;
};

export type PerfJitTotals = {
  tier1: PerfJitTierTotals;
  tier2: PerfJitTier2Totals;
  cache: PerfJitCacheTotals;
  deopt: PerfJitDeoptTotals;
};

export type PerfJitRolling = {
  windowMs: number;
  cacheHitRate: number;
  compileMsPerSec: number;
  blocksCompiledPerSec: number;
};

export type PerfJitSnapshot = {
  enabled: boolean;
  totals: PerfJitTotals;
  rolling: PerfJitRolling;
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
  hostJsHeapLimitBytes?: number;

  guestRamBytes?: number;

  wasmMemoryBytes?: number;
  wasmMemoryPages?: number;
  wasmMemoryMaxPages?: number;

  gpuEstimatedBytes?: number;
  jitCodeCacheBytes?: number;
  shaderCacheBytes?: number;

  jit?: PerfJitSnapshot;

  peakHostJsHeapUsedBytes?: number;
  peakWasmMemoryBytes?: number;
  peakGpuEstimatedBytes?: number;

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
