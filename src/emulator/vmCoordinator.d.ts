export type VmState = "stopped" | "starting" | "running" | "paused" | "error";

export type VmCoordinatorConfig = {
  cpu?: {
    watchdogTimeoutMs?: number;
    maxSliceMs?: number;
    maxInstructionsPerSlice?: number;
    backgroundThrottleMs?: number;
  };
  guestRamBytes?: number;
  limits?: {
    maxGuestRamBytes?: number;
    maxDiskCacheBytes?: number;
    maxShaderCacheBytes?: number;
  };
  autoSaveSnapshotOnCrash?: boolean;
};

export type VmCoordinatorStartOptions = {
  mode?: "cooperativeInfiniteLoop" | "nonYieldingLoop" | "crash";
};

export type VmCacheKind = "disk" | "shader";

export type VmCacheWriteResult = {
  type: "cacheWriteResult";
  requestId: number;
  cache: VmCacheKind;
  ok: boolean;
  stats?: { diskCacheBytes: number; shaderCacheBytes: number };
  error?: unknown;
};

export class VmCoordinator extends EventTarget {
  constructor(options?: { config?: VmCoordinatorConfig; workerUrl?: URL });

  static loadSavedCrashSnapshot(): Promise<{ savedTo: string; snapshot: unknown } | null>;
  static clearSavedCrashSnapshot(): Promise<void>;

  readonly config: VmCoordinatorConfig;
  state: VmState;
  lastHeartbeatAt: number;
  lastHeartbeat: unknown;
  lastSnapshot: unknown;
  lastSnapshotSavedTo: string | null;

  start(options?: VmCoordinatorStartOptions): Promise<void>;
  pause(): Promise<void>;
  resume(): Promise<void>;
  step(): Promise<void>;
  setBackgrounded(backgrounded: boolean): void;
  requestSnapshot(options?: { reason?: string }): Promise<unknown>;
  writeCacheEntry(options: { cache: VmCacheKind; sizeBytes: number; key?: string }): Promise<VmCacheWriteResult>;
  shutdown(): void;
  reset(): void;
}
