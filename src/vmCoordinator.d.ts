export type VmState = 'stopped' | 'starting' | 'running' | 'paused' | 'error';

export type VmCoordinatorConfig = {
  cpu?: {
    watchdogTimeoutMs?: number;
    ackTimeoutMs?: number;
    maxSliceMs?: number;
    maxInstructionsPerSlice?: number;
    backgroundThrottleMs?: number;
  };
  autoSaveSnapshotOnCrash?: boolean;
  guestRamBytes?: number;
  limits?: {
    maxGuestRamBytes?: number;
    maxDiskCacheBytes?: number;
    maxShaderCacheBytes?: number;
  };
};

export type VmCoordinatorStartOptions = {
  mode?: 'cooperativeInfiniteLoop' | 'nonYieldingLoop' | 'crash';
};

export type VmCacheKind = 'disk' | 'shader';

export type VmCacheWriteResult = {
  type: 'cacheWriteResult';
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

export type VmHeartbeat = {
  type: 'heartbeat';
  at: number;
  executed: number;
  totalInstructions: number;
  pc: number;
  resources: VmResourcesSnapshot;
};

export type VmHeartbeatSnapshot = {
  reason: 'heartbeat';
  capturedAt: number;
  cpu: { pc: number; totalInstructions: number };
  resources: VmResourcesSnapshot;
};

export class VmCoordinator extends EventTarget {
  constructor(options?: { config?: VmCoordinatorConfig; workerUrl?: URL });

  readonly config: VmCoordinatorConfig;
  state: VmState;
  lastHeartbeatAt: number;
  lastHeartbeat: VmHeartbeat | null;
  // Heartbeat snapshots are sanitized, but other snapshot shapes may still be worker-defined.
  lastSnapshot: VmHeartbeatSnapshot | unknown;

  start(options?: VmCoordinatorStartOptions): Promise<void>;
  pause(): Promise<void>;
  resume(): Promise<void>;
  step(): Promise<void>;
  setBackgrounded(backgrounded: boolean): void;
  requestSnapshot(options?: { reason?: string }): Promise<unknown>;
  writeCacheEntry(options: { cache: VmCacheKind; sizeBytes: number; key?: string }): Promise<VmCacheWriteResult>;
  shutdown(): Promise<void>;
  reset(): Promise<void>;
}
