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

export type VmResourcesSnapshot = {
  guestRamBytes: number;
  diskCacheBytes: number;
  shaderCacheBytes: number;
};

export type VmMicSnapshot = {
  rms: number;
  dropped: number;
  sampleRate: number;
};

export type VmHeartbeat = {
  type: "heartbeat";
  at: number;
  executed: number;
  pc: number;
  totalInstructions: number;
  resources: VmResourcesSnapshot;
  mic: VmMicSnapshot | null;
};

export type VmHeartbeatSnapshot = {
  reason: "heartbeat";
  capturedAt: number;
  cpu: { pc: number; totalInstructions: number };
  resources: VmResourcesSnapshot;
};

export type VmStructuredError = {
  name: string;
  code: string;
  message: string;
  details?: unknown;
  suggestion?: unknown;
  stack?: string;
};

export class VmCoordinator extends EventTarget {
  constructor(options?: { config?: VmCoordinatorConfig; workerUrl?: URL | null });

  static loadSavedCrashSnapshot(): Promise<{ savedTo: string; snapshot: unknown } | null>;
  static clearSavedCrashSnapshot(): Promise<void>;

  readonly config: VmCoordinatorConfig;
  state: VmState;
  lastHeartbeatAt: number;
  lastHeartbeat: VmHeartbeat | null;
  // Heartbeat snapshots are sanitized, but other snapshot shapes may still be worker-defined.
  lastSnapshot: VmHeartbeatSnapshot | unknown;
  lastSnapshotSavedTo: string | null;
  lastError: { error: VmStructuredError; snapshot?: unknown } | null;

  start(options?: VmCoordinatorStartOptions): Promise<void>;
  pause(): Promise<void>;
  resume(): Promise<void>;
  step(): Promise<void>;
  setBackgrounded(backgrounded: boolean): void;
  setMicrophoneRingBuffer(ringBuffer: SharedArrayBuffer | null, options?: { sampleRate?: number }): void;
  requestSnapshot(options?: { reason?: string }): Promise<unknown>;
  writeCacheEntry(options: { cache: VmCacheKind; sizeBytes: number; key?: string }): Promise<VmCacheWriteResult>;
  shutdown(): void;
  reset(): void;
}
