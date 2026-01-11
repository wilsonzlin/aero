import type { ResponsivenessHudSnapshot } from './responsiveness';
import type { PerfChannel } from './shared.js';

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
  mipsP95?: number;

  lastFrameTimeMs?: number;
  lastMips?: number;

  breakdownAvgMs?: PerfTimeBreakdownMs;

  drawCallsPerFrame?: number;
  pipelineSwitchesPerFrame?: number;
  ioBytesPerSec?: number;
  gpuUploadBytesPerSec?: number;

  gpuTimingSupported?: boolean;
  gpuTimingEnabled?: boolean;

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
  responsiveness?: ResponsivenessHudSnapshot;

  capture: PerfCaptureState;
};

export interface PerfApi {
  getHudSnapshot(out: PerfHudSnapshot): PerfHudSnapshot;
  setHudActive(active: boolean): void;

  /**
   * Returns the installed SharedArrayBuffer perf channel config.
   *
   * This is used by the runtime coordinator to plumb per-worker ring buffers to
   * Web Workers. Environments without SharedArrayBuffer support should return
   * null.
   */
  getChannel(): PerfChannel | null;

  noteInputCaptured?(id: number, tCaptureMs?: number): void;
  noteInputInjected?(id: number, tInjectedMs?: number, queueDepth?: number, queueOldestCaptureMs?: number | null): void;
  noteInputConsumed?(id: number, tConsumedMs?: number, queueDepth?: number, queueOldestCaptureMs?: number | null): void;
  notePresent?(tPresentMs?: number): void;

  captureStart(): void;
  captureStop(): void;
  captureReset(): void;
  export(): unknown;
}
