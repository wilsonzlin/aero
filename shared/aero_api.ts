import type { AeroPhase, AeroStatusSnapshot } from './aero_status.ts';

export interface AeroPerfApi {
  export: () => unknown;
  getStats?: () => unknown;
  setEnabled?: (enabled: boolean) => void;
}

export interface WebGpuBenchOptions {
  frames?: number;
  warmupFrames?: number;
  warmup_frames?: number;
  width?: number;
  height?: number;
  drawCallsPerFrame?: number;
  draw_calls_per_frame?: number;
  pipelineSwitchesPerFrame?: number;
  pipeline_switches_per_frame?: number;
  compute?: boolean;
  computeWorkgroups?: number;
  compute_workgroups?: number;
}

export interface WebGpuBenchAdapterInfo {
  vendor: string | null;
  architecture: string | null;
  device: string | null;
  description: string | null;
}

export type WebGpuBenchResult =
  | {
      supported: false;
      reason: string;
    }
  | {
      supported: true;
      adapter: WebGpuBenchAdapterInfo | null;
      capabilities: {
        timestamp_query: boolean;
      };
      frames: number;
      fps: number;
      draw_calls_per_frame: number;
      pipeline_switches_per_frame: number;
      cpu_encode_time_ms: {
        avg: number;
        p95: number;
      };
      gpu_time_ms: number | null;
      compute: {
        enabled: boolean;
        workgroups: number;
      };
    };

export interface AeroBenchApi {
  runWebGpuBench?: (opts?: WebGpuBenchOptions) => Promise<WebGpuBenchResult>;
  runMicrobenchSuite?: (opts?: unknown) => Promise<unknown>;
  runStorageBench?: (opts?: unknown) => Promise<unknown>;
  runGuestCpuBench?: (opts?: unknown) => Promise<unknown>;
}

export interface AeroNetTraceApi {
  isEnabled: () => boolean;
  enable: () => void;
  disable: () => void;
  downloadPcapng: () => Promise<Uint8Array>;
  /**
   * Non-draining snapshot export (if supported by the runtime).
   *
   * Unlike `downloadPcapng()`, this does not clear the in-memory capture buffer.
   */
  exportPcapng?: () => Promise<Uint8Array>;
  clear?: () => void | Promise<void>;
  /**
   * Legacy alias for clearing the capture (some older hosts use this name).
   */
  clearCapture?: () => void;
  getStats?: () => unknown | Promise<unknown>;
}

export interface AeroGlobalApi {
  perf?: AeroPerfApi;
  bench?: AeroBenchApi;
  netTrace?: AeroNetTraceApi;

  /**
   * Host-visible status that macrobench scenarios can wait on.
   */
  status?: AeroStatusSnapshot;

  /**
   * Event bus used by the host to signal milestones (e.g. `desktop_ready`).
   */
  events?: EventTarget;

  setPhase?: (phase: AeroPhase) => void;
  waitForPhase?: (phase: AeroPhase, options?: { timeoutMs?: number }) => Promise<void>;
  emitEvent?: (name: string, detail?: unknown) => void;
  waitForEvent?: <T = unknown>(name: string, options?: { timeoutMs?: number }) => Promise<T>;
}
