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
  /**
   * Browser-installed bench hooks.
   *
   * These are intentionally typed as `unknown` in `shared/` to avoid
   * cross-package dependencies on the `web/` runtime's bench types.
   */
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

export type IoInputTelemetrySnapshot = {
  batchesReceived: number;
  batchesProcessed: number;
  batchesDropped: number;
  keyboardBackendSwitches: number;
  mouseBackendSwitches: number;
};

export type BootDeviceKind = "hdd" | "cdrom";

export type BootDiskSelectionSnapshot = {
  mounts: { hddId?: string; cdId?: string };
  bootDevice?: BootDeviceKind;
};

export type MachineCpuBootConfigSnapshot = {
  bootDrive: number;
  cdBootDrive: number;
  bootFromCdIfPresent: boolean;
};

export interface AeroDebugApi {
  /**
   * Reads input telemetry counters from the given shared status view.
   *
   * Ownership:
   * - `vmRuntime=legacy`: written by the I/O worker (input is injected there)
   * - `vmRuntime=machine`: written by the machine CPU worker (input is injected there)
   *
   * In the browser runtime, `status` is an `Int32Array` view into the shared
   * `control` SharedArrayBuffer (`StatusIndex`).
   */
  readIoInputTelemetry?: (status: Int32Array) => IoInputTelemetrySnapshot;

  /**
   * Returns input telemetry for the active runtime (or null if no VM is running / shared status is
   * unavailable).
   */
  getIoInputTelemetry?: () => IoInputTelemetrySnapshot | null;

  /**
   * Returns the current boot disk selection (mount IDs + requested boot-device policy) when
   * available.
   */
  getBootDisks?: () => BootDiskSelectionSnapshot | null;

  /**
   * Returns what firmware actually booted from for the current machine runtime session (CD vs HDD),
   * or null if unknown/unavailable (including during reboot/reattach transitions before the CPU
   * worker re-reports the new boot session).
   */
  getMachineCpuActiveBootDevice?: () => BootDeviceKind | null;

  /**
   * Returns the machine CPU worker's BIOS boot configuration (boot drive number + CD-first state),
   * or null if unknown/unavailable (including during reboot/reattach transitions before the CPU
   * worker re-reports the new boot session).
   */
  getMachineCpuBootConfig?: () => MachineCpuBootConfigSnapshot | null;
}

export interface AeroGlobalApi {
  perf?: AeroPerfApi;
  bench?: AeroBenchApi;
  netTrace?: AeroNetTraceApi;
  debug?: AeroDebugApi;

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
