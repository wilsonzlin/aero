import type { ResponsivenessExport } from './responsiveness';
import type { MemoryTelemetryExport } from './memory';
import type { PerfJitSnapshot } from './types';

export type PerfCaptureRecord = {
  tMs: number;
  frameTimeMs: number;
  instructions: number | null;
  cpuMs: number | null;
  gpuMs: number | null;
  ioMs: number | null;
  jitMs: number | null;
  drawCalls: number | null;
  ioBytes: number | null;
};

export type PerfExport = {
  kind: 'aero-perf-capture';
  version: 1;
  startUnixMs: number | null;
  durationMs: number;
  droppedRecords: number;
  guestRamBytes: number | null;
  jit: PerfJitSnapshot;
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
  records: PerfCaptureRecord[];
};
