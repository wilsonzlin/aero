import type { AeroPhase, AeroStatusSnapshot } from './aero_status.ts';

export interface AeroPerfApi {
  export: () => unknown;
  getStats?: () => unknown;
  setEnabled?: (enabled: boolean) => void;
}

export interface AeroGlobalApi {
  perf?: AeroPerfApi;

  /**
   * Host-visible status that macrobench scenarios can wait on.
   */
  status?: AeroStatusSnapshot;

  /**
   * Event bus used by the host to signal milestones (e.g. `desktop_ready`).
   */
  events?: EventTarget;

  setPhase?: (phase: AeroPhase) => void;
  emitEvent?: (name: string, detail?: unknown) => void;
  waitForEvent?: <T = unknown>(name: string, options?: { timeoutMs?: number }) => Promise<T>;
}

