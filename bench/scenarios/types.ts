import type { AeroPhase } from '../../shared/aero_status.ts';

export type ScenarioKind = 'micro' | 'macro';

export type DiskImageSource =
  | {
      kind: 'path';
      path: string;
    }
  | {
      kind: 'url';
      url: string;
    };

export type DiskImageRequirement = 'none' | 'optional' | 'required';

export interface ScenarioRequirements {
  webgpu?: boolean;
  opfs?: boolean;
  crossOriginIsolated?: boolean;
  diskImage?: DiskImageRequirement;
}

export interface HostCapabilities {
  webgpu: boolean;
  opfs: boolean;
  crossOriginIsolated: boolean;
}

export interface EmulatorCapabilities {
  systemBoot: boolean;
  perfExport: boolean;
  screenshots: boolean;
  trace: boolean;
  statusApi: boolean;
}

export interface EmulatorDriver {
  readonly capabilities: EmulatorCapabilities;
  configure(options: Record<string, unknown>): Promise<void>;
  attachDiskImage(source: DiskImageSource): Promise<void>;
  start(): Promise<void>;
  stop(): Promise<void>;
  eval<T>(expression: string): Promise<T>;
  screenshotPng(): Promise<Uint8Array>;
  exportPerf?(): Promise<unknown>;
  startTrace?(): Promise<void>;
  stopTrace?(): Promise<Uint8Array>;
  sendInput?(payload: unknown): Promise<void>;
}

export type MetricUnit = 'ms' | 'fps' | 'count';

export interface Metric {
  id: string;
  unit: MetricUnit;
  value: number;
}

export const METRIC_BOOT_TIME_MS = 'boot_time_ms';
export const METRIC_DESKTOP_FPS = 'desktop_fps';
export const METRIC_APP_LAUNCH_TIME_MS = 'app_launch_time_ms';
export const METRIC_INPUT_LATENCY_MS = 'input_latency_ms';

export class MetricsRecorder {
  readonly #metrics = new Map<string, Metric>();

  set(metric: Metric) {
    this.#metrics.set(metric.id, metric);
  }

  setMs(id: string, value: number) {
    this.set({ id, unit: 'ms', value });
  }

  setFps(id: string, value: number) {
    this.set({ id, unit: 'fps', value });
  }

  snapshot(): Metric[] {
    return [...this.#metrics.values()].sort((a, b) => a.id.localeCompare(b.id));
  }
}

export type ArtifactKind = 'perf_export' | 'screenshot' | 'trace' | 'report' | 'other';

export interface ArtifactManifestEntry {
  kind: ArtifactKind;
  path: string;
}

export interface ArtifactWriter {
  readonly rootDir: string;
  writeJson(path: string, data: unknown, kind?: ArtifactKind): Promise<void>;
  writeBinary(path: string, data: Uint8Array, kind?: ArtifactKind): Promise<void>;
  manifest(): ArtifactManifestEntry[];
}

export interface MilestoneClient {
  waitForPhase(phase: AeroPhase, options?: { timeoutMs?: number }): Promise<void>;
  waitForEvent(name: string, options?: { timeoutMs?: number }): Promise<void>;
  waitForStableScreen(options?: {
    timeoutMs?: number;
    intervalMs?: number;
    stableCount?: number;
  }): Promise<void>;
  captureScreenshot(name: string): Promise<void>;
}

export interface RunnerConfig {
  scenarioId: string;
  outDir: string;
  diskImage?: DiskImageSource;
  trace: boolean;
}

export interface ScenarioContext {
  runId: string;
  config: RunnerConfig;
  host: HostCapabilities;
  emulator: EmulatorDriver;
  artifacts: ArtifactWriter;
  metrics: MetricsRecorder;
  milestones: MilestoneClient;
  log(message: string): void;
}

export interface Scenario {
  id: string;
  name: string;
  kind: ScenarioKind;
  requirements?: ScenarioRequirements;
  setup?(ctx: ScenarioContext): Promise<void>;
  run(ctx: ScenarioContext): Promise<void>;
  collect?(ctx: ScenarioContext): Promise<void>;
  teardown?(ctx: ScenarioContext): Promise<void>;
}

export type ScenarioStatus = 'ok' | 'skipped' | 'error';

export interface ScenarioReport {
  runId: string;
  scenarioId: string;
  scenarioName: string;
  kind: ScenarioKind;
  status: ScenarioStatus;
  startedAtMs: number;
  finishedAtMs: number;
  metrics: Metric[];
  artifacts: ArtifactManifestEntry[];
  skipReason?: string;
  error?: {
    message: string;
    stack?: string;
  };
}

export class ScenarioSkippedError extends Error {
  readonly reason: string;

  constructor(reason: string) {
    super(`Scenario skipped: ${reason}`);
    this.reason = reason;
  }
}

