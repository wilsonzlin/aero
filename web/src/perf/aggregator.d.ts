import type { PerfChannel } from "./shared";
import type { SpscRingBuffer } from "./ring_buffer";

export type AggregatedFrame = {
  frameId: number;
  tUs?: number;
  frameUs: number;
  cpuUs: number;
  gpuUs: number;
  ioUs: number;
  jitUs: number;
  instructions: bigint;
  memoryBytes: bigint;
  drawCalls: number;
  ioReadBytes: number;
  ioWriteBytes: number;
  hasMainFrameTime: boolean;
};

export type PerfStats = {
  windowSize: number;
  frames: number;
  avgFrameMs: number;
  p50FrameMs: number;
  p95FrameMs: number;
  p99FrameMs: number;
  avgFps: number;
  fps1pLow: number;
  avgMips: number;
};

export class PerfAggregator {
  constructor(
    channel: PerfChannel,
    options?: { windowSize?: number; captureSize?: number; maxDrainPerBuffer?: number },
  );

  readonly channel: PerfChannel;
  readonly windowSize: number;
  readonly captureSize: number;
  readonly maxDrainPerBuffer: number;

  readonly frames: Map<number, AggregatedFrame>;
  readonly completedFrameIds: number[];
  readonly readers: Map<number, SpscRingBuffer>;

  readonly recordCountsByWorkerKind: Map<number, number>;
  readonly totalRecordsDrained: number;
  readonly totalFrameSampleRecords: number;

  drain(): void;
  getStats(): PerfStats;
  export(): unknown;
}

export function collectEnvironmentMetadata(): unknown;
export function collectBuildMetadata(): unknown;

