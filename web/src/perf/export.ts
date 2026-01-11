import type { ResponsivenessExport } from './responsiveness';
import type { MemoryTelemetryExport } from './memory';
import type { PerfJitSnapshot } from './types';

export type PerfCaptureRecord = {
  tMs: number;
  frameTimeMs: number;
  instructions: number | string | null;
  cpuMs: number | null;
  gpuMs: number | null;
  ioMs: number | null;
  jitMs: number | null;
  drawCalls: number | null;
  ioBytes: number | null;
};

export type PerfBufferStats = {
  workerKind: number;
  worker: string;
  capacity: number;
  recordSize: number;
  droppedRecords: number;
  drainedRecords: number;
};

export type PerfBuildMetadata = {
  git_sha: string | null;
  mode: string | null;
  features?: Record<string, unknown> | null;
};

export type PerfEnvironmentMetadata = {
  now_epoch_ms: number;
  userAgent: string | null;
  platform: string | null;
  hardwareConcurrency: number | null;
  devicePixelRatio: number | null;
  webgpu: boolean;
};

export type PerfCaptureMetadata = {
  startUnixMs: number | null;
  endUnixMs: number | null;
  durationMs: number;
};

export type PerfCaptureControl = {
  startFrameId: number | null;
  endFrameId: number | null;
  droppedRecords: number;
  records: number;
};

export type PerfExportV2 = {
  kind: 'aero-perf-capture';
  version: 2;
  build: PerfBuildMetadata;
  env: PerfEnvironmentMetadata;
  capture: PerfCaptureMetadata;
  capture_control: PerfCaptureControl;
  buffers: PerfBufferStats[];
  guestRamBytes: number | null;
  memory: MemoryTelemetryExport;
  summary: {
    frameTime: unknown;
    mipsAvg: number | null;
  };
  frameTime: {
    summary: unknown;
    stats: unknown;
  };
  responsiveness: ResponsivenessExport;
  jit: PerfJitSnapshot;
  records: PerfCaptureRecord[];
  benchmarks?: Record<string, unknown>;
};

export type PerfExport = PerfExportV2;

export const JIT_DISABLED_SNAPSHOT: PerfJitSnapshot = {
  enabled: false,
  totals: {
    tier1: { blocksCompiled: 0, compileMs: 0 },
    tier2: { blocksCompiled: 0, compileMs: 0, passesMs: { constFold: 0, dce: 0, regalloc: 0 } },
    cache: { lookupHit: 0, lookupMiss: 0, capacityBytes: 0, usedBytes: 0 },
    deopt: { count: 0, guardFail: 0 },
  },
  rolling: { windowMs: 0, cacheHitRate: 0, compileMsPerSec: 0, blocksCompiledPerSec: 0 },
};
