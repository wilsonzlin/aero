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
  renderPasses: number;
  pipelineSwitches: number;
  bindGroupChanges: number;
  uploadBytes: bigint;
  cpuTranslateUs: number;
  cpuEncodeUs: number;
  gpuTimeUs: number;
  gpuTimeValid: boolean;
  gpuTimingSupported: boolean;
  gpuTimingEnabled: boolean;
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
  p999FrameMs: number;
  avgFps: number;
  fpsMedian: number;
  fpsP95: number;
  fps1pLow: number;
  fps0_1pLow: number;
  varianceFrameMs2: number;
  stdevFrameMs: number;
  covFrameTime: number;
  avgMips: number;
  p95Mips: number;

  drawCallsPerFrame: number;
  renderPassesPerFrame: number;
  pipelineSwitchesPerFrame: number;
  bindGroupChangesPerFrame: number;
  gpuUploadBytesPerSec: number;
  cpuTranslateMs: number;
  cpuEncodeMs: number;
  gpuTimeAvgMs: number | null;
  gpuTimingSupported: boolean;
  gpuTimingEnabled: boolean;
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
  readonly totalGraphicsSampleRecords: number;

  hotspots: unknown[];

  drain(): void;
  getStats(): PerfStats;
  setHotspots(hotspots: unknown[]): void;
  export(): unknown;
}

export function collectEnvironmentMetadata(): unknown;
export function collectBuildMetadata(): unknown;
