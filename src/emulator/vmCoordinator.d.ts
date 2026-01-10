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

export class VmCoordinator extends EventTarget {
  constructor(options?: { config?: VmCoordinatorConfig; workerUrl?: URL });

  readonly config: VmCoordinatorConfig;
  state: VmState;
  lastHeartbeatAt: number;
  lastHeartbeat: unknown;
  lastSnapshot: unknown;

  start(options?: VmCoordinatorStartOptions): Promise<void>;
  pause(): Promise<void>;
  resume(): Promise<void>;
  step(): Promise<void>;
  setBackgrounded(backgrounded: boolean): void;
  requestSnapshot(options?: { reason?: string }): Promise<unknown>;
  shutdown(): void;
  reset(): void;
}

