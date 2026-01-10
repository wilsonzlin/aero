export type FrameTimeStatsOptions = {
  keepLastNSamples?: number;
  histogramSubBucketCount?: number;
  histogramMaxExponent?: number;
};

export type FrameTimeStatsSummary = {
  frames: number;
  totalTimeMs: number;
  meanFrameTimeMs: number;
  minFrameTimeMs: number;
  maxFrameTimeMs: number;
  varianceFrameTimeMs2: number;
  stdevFrameTimeMs: number;
  covFrameTime: number;
  frameTimeP50Ms: number;
  frameTimeP95Ms: number;
  frameTimeP99Ms: number;
  frameTimeP999Ms: number;
  fpsAvg: number;
  fpsMedian: number;
  fpsP95: number;
  fps1Low: number;
  fps0_1Low: number;
};

export class FrameTimeStats {
  constructor(options?: FrameTimeStatsOptions);

  get frames(): number;

  pushFrameTimeMs(frameTimeMs: number): void;
  merge(other: FrameTimeStats): void;
  clear(): void;
  getRecentFrameTimesMs(): number[];
  summary(): FrameTimeStatsSummary;
  toJSON(): unknown;
  static fromJSON(data: unknown): FrameTimeStats;
}

export type LogHistogramOptions = {
  subBucketCount?: number;
  maxExponent?: number;
};

export class LogHistogram {
  constructor(options?: LogHistogramOptions);

  get subBucketCount(): number;
  get totalCount(): number;
  get min(): number;
  get max(): number;

  record(value: number, count?: number): void;
  merge(other: LogHistogram): void;
  quantile(q: number): number;
  clear(): void;
  toJSON(): unknown;
  static fromJSON(data: unknown): LogHistogram;
}

export class RunningStats {
  get count(): number;
  get min(): number;
  get max(): number;
  get sum(): number;
  get mean(): number;
  get variancePopulation(): number;
  get varianceSample(): number;
  get stdevPopulation(): number;
  get stdevSample(): number;
  get coefficientOfVariation(): number;

  push(value: number): void;
  merge(other: RunningStats): void;
  clear(): void;
  toJSON(): unknown;
  static fromJSON(data: unknown): RunningStats;
}

export class FixedRingBuffer<T = unknown> {
  constructor(capacity: number);

  get capacity(): number;
  get size(): number;

  push(value: T): void;
  clear(): void;
  toArray(): T[];
}

export function frameTimeMsToFps(frameTimeMs: number): number;
export function fpsToFrameTimeMs(fps: number): number;
export function msToUs(ms: number): number;
export function usToMs(us: number): number;

export function computeMips(args: { instructions: number; elapsedMs: number }): number;
