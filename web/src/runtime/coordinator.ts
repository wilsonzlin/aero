import type { AeroConfig } from "../config/aero_config";
import { openRingByKind } from "../ipc/ipc";
import { ringCtrl } from "../ipc/layout";
import { RingBuffer } from "../ipc/ring_buffer";
import { decodeEvent, encodeCommand, type Command, type Event } from "../ipc/protocol";
import { perf } from "../perf/perf";
import { WorkerKind } from "../perf/record.js";
import type { PerfChannel } from "../perf/shared.js";
import { detectPlatformFeatures, type PlatformFeatureReport } from "../platform/features";
import {
  WORKER_ROLES,
  type WorkerRole,
  CPU_WORKER_DEMO_GUEST_COUNTER_INDEX,
  IO_IPC_NET_RX_QUEUE_KIND,
  IO_IPC_NET_TX_QUEUE_KIND,
  StatusIndex,
  allocateSharedMemorySegments,
  checkSharedMemorySupport,
  computeGuestRamLayout,
  createIoIpcSab,
  createSharedMemoryViews,
  ringRegionsForWorker,
  setReadyFlag,
  type SharedMemoryViews,
} from "./shared_layout";
import {
  type ConfigAckMessage,
  type ConfigUpdateMessage,
  type NetTraceClearMessage,
  type NetTraceDisableMessage,
  type NetTraceEnableMessage,
  type NetTraceExportPcapngMessage,
  type NetTracePcapngMessage,
  type NetTraceStatusMessage,
  type NetTraceStatusResponseMessage,
  type NetTraceTakePcapngMessage,
  MessageType,
  type ProtocolMessage,
  type SetAudioRingBufferMessage,
  type SetMicrophoneRingBufferMessage,
  type CursorSetImageMessage,
  type CursorSetStateMessage,
  type WorkerInitMessage,
  type WasmReadyMessage,
} from "./protocol";
import type {
  VmSnapshotCpuStateMessage,
  VmSnapshotCpuStateSetMessage,
  VmSnapshotPausedMessage,
  VmSnapshotResumedMessage,
  VmSnapshotRestoredMessage,
  VmSnapshotSavedMessage,
  VmSnapshotSerializedError,
} from "./snapshot_protocol";
import type { WasmVariant } from "./wasm_context";
import { precompileWasm } from "./wasm_preload";
import {
  GPU_PROTOCOL_NAME,
  GPU_PROTOCOL_VERSION,
  type GpuRuntimeCursorSetImageMessage,
  type GpuRuntimeCursorSetStateMessage,
} from "../ipc/gpu-protocol";
const GPU_MESSAGE_BASE = { protocol: GPU_PROTOCOL_NAME, protocolVersion: GPU_PROTOCOL_VERSION } as const;

export type WorkerState = "starting" | "ready" | "failed" | "stopped";

export interface WorkerStatus {
  state: WorkerState;
  error?: string;
}

export interface WorkerWasmStatus {
  variant: WasmVariant;
  value: number;
}

/**
 * Shared ring-buffer attachment forwarding policy for audio I/O.
 *
 * These are intentionally explicit because the AudioWorklet ↔ emulator rings are
 * single-producer/single-consumer (SPSC) structures:
 *
 * - Audio output ring: producer = emulator (exactly ONE worker), consumer = AudioWorklet.
 * - Microphone ring: producer = AudioWorklet/ScriptProcessor, consumer = emulator (exactly ONE worker).
 *
 * Accidentally attaching the same SharedArrayBuffer ring to multiple emulator workers
 * creates multi-producer/multi-consumer access patterns and corrupts the shared
 * read/write indices (undefined behaviour, underruns/overruns, etc).
 *
 * The coordinator therefore owns the policy for which worker(s) receive the SAB
 * attachments.
 */
export type RingBufferOwner = "cpu" | "io" | "both" | "none";

const AUDIO_RING_WORKER_ROLES = ["cpu", "io"] as const;
type AudioRingWorkerRole = (typeof AUDIO_RING_WORKER_ROLES)[number];

export type VmLifecycleState = "stopped" | "starting" | "running" | "restarting" | "resetting" | "poweredOff" | "failed";

export type WorkerCoordinatorFatalKind =
  | "start_failed"
  | "worker_error"
  | "worker_message_error"
  | "worker_reported_error"
  | "ipc_panic"
  | "ipc_triple_fault"
  | "gpu_fatal";

export type WorkerCoordinatorNonFatalKind = "gpu_device_lost" | "gpu_error" | "ipc_log" | "net_error";

export interface WorkerCoordinatorFatalDetail {
  kind: WorkerCoordinatorFatalKind;
  role?: WorkerRole;
  message: string;
  stack?: string;
  atMs: number;
}

export interface WorkerCoordinatorNonFatalDetail {
  kind: WorkerCoordinatorNonFatalKind;
  role?: WorkerRole;
  message: string;
  stack?: string;
  atMs: number;
}

export interface WorkerCoordinatorStateChangeDetail {
  prev: VmLifecycleState;
  next: VmLifecycleState;
  reason?: string;
  atMs: number;
}

export interface WorkerCoordinatorEventMap {
  fatal: WorkerCoordinatorFatalDetail;
  nonfatal: WorkerCoordinatorNonFatalDetail;
  statechange: WorkerCoordinatorStateChangeDetail;
}

interface WorkerInfo {
  role: WorkerRole;
  instanceId: number;
  worker: Worker;
  status: WorkerStatus;
  commandRing: RingBuffer;
  eventRing: RingBuffer;
}

type GpuWorkerGpuErrorMessage = {
  type: "gpu_error";
  fatal: boolean;
  error: { message?: string; stack?: string };
};

type GpuWorkerErrorEventMessage = {
  type: "gpu_error_event";
  event: { category?: string; message?: string };
};

type PendingNetTraceRequest = {
  resolve: (bytes: Uint8Array<ArrayBuffer>) => void;
  reject: (err: Error) => void;
  timeout: number;
};

type PendingNetTraceStatusRequest = {
  resolve: (msg: NetTraceStatusResponseMessage) => void;
  reject: (err: Error) => void;
  timeout: number;
};

function nowMs(): number {
  return typeof performance !== "undefined" && typeof performance.now === "function" ? performance.now() : Date.now();
}

function maybeGetHudPerfChannel(): PerfChannel | null {
  const aero = (globalThis as unknown as { aero?: unknown }).aero as { perf?: unknown } | undefined;
  const perfApi = aero?.perf as { getChannel?: () => PerfChannel | null } | undefined;
  if (perfApi && typeof perfApi.getChannel === "function") {
    return perfApi.getChannel();
  }
  return null;
}

function workerRoleToPerfWorkerKind(role: WorkerRole): number {
  switch (role) {
    case "cpu":
      return WorkerKind.CPU;
    case "gpu":
      return WorkerKind.GPU;
    case "io":
      return WorkerKind.IO;
    case "jit":
      return WorkerKind.JIT;
    case "net":
      return WorkerKind.NET;
    default: {
      const neverRole: never = role;
      throw new Error(`Unknown worker role: ${String(neverRole)}`);
    }
  }
}

function formatWorkerError(ev: ErrorEvent): { message: string; stack?: string } {
  const base = ev.message || "Worker error";
  const location =
    ev.filename && typeof ev.lineno === "number" && typeof ev.colno === "number"
      ? `${ev.filename}:${ev.lineno}:${ev.colno}`
      : ev.filename
        ? ev.filename
        : "";
  const message = location ? `${base} @ ${location}` : base;
  const stack = (ev.error as { stack?: unknown } | null | undefined)?.stack;
  return { message, stack: typeof stack === "string" ? stack : undefined };
}

function isGpuWorkerGpuErrorMessage(data: unknown): data is GpuWorkerGpuErrorMessage {
  if (!data || typeof data !== "object") return false;
  const msg = data as { type?: unknown; fatal?: unknown; error?: unknown };
  return msg.type === "gpu_error" && typeof msg.fatal === "boolean" && typeof msg.error === "object" && msg.error !== null;
}

function isGpuWorkerErrorEventMessage(data: unknown): data is GpuWorkerErrorEventMessage {
  if (!data || typeof data !== "object") return false;
  const msg = data as { type?: unknown; event?: unknown };
  return msg.type === "gpu_error_event" && typeof msg.event === "object" && msg.event !== null;
}

class RestartBackoff {
  private attempts = 0;

  constructor(
    private readonly baseDelayMs: number,
    private readonly maxDelayMs: number,
    private readonly jitterFraction = 0.2,
  ) {}

  reset(): void {
    this.attempts = 0;
  }

  nextDelayMs(): number {
    this.attempts += 1;
    const unclamped = this.baseDelayMs * 2 ** Math.max(0, this.attempts - 1);
    const delay = Math.min(this.maxDelayMs, unclamped);
    const jitter = delay * this.jitterFraction;
    const randomized = delay + (Math.random() * 2 - 1) * jitter;
    return Math.max(0, Math.round(randomized));
  }

  getAttemptCount(): number {
    return this.attempts;
  }
}

export class WorkerCoordinator {
  private readonly events = new EventTarget();
  private shared?: SharedMemoryViews;
  private workers: Partial<Record<WorkerRole, WorkerInfo>> = {};
  private runId = 0;
  private nextWorkerInstanceId = 1;
  private frameStateSab?: SharedArrayBuffer;
  private platformFeatures: PlatformFeatureReport | null = null;
  private nextCmdSeq = 1;
  private nextSnapshotRequestId = 1;
  private snapshotInFlight = false;

  private lastHeartbeatFromRing = 0;
  private wasmStatus: Partial<Record<WorkerRole, WorkerWasmStatus>> = {};

  private readonly serialDecoder = new TextDecoder();
  private serialOutputText = "";
  private serialOutputBytes = 0;
  private resetRequestCount = 0;
  private lastResetRequestAtMs = 0;

  // Optional SharedArrayBuffer-backed microphone ring buffer attachment. This
  // is set by the UI and forwarded to exactly one emulation worker (SPSC).
  // IMPORTANT: `micSampleRate` is the *actual* capture sample rate
  // (AudioContext.sampleRate), not the requested rate.
  private micRingBuffer: SharedArrayBuffer | null = null;
  private micSampleRate = 0;
  // Optional SharedArrayBuffer-backed audio output ring buffer attachment. This
  // is set by the UI and forwarded to exactly one emulation worker (SPSC).
  private audioRingBuffer: SharedArrayBuffer | null = null;
  private audioCapacityFrames = 0;
  private audioChannelCount = 0;
  private audioDstSampleRate = 0;
  // Tracks which worker currently owns the SPSC producer/consumer roles for the
  // SharedArrayBuffer rings. This lets the coordinator enforce "detach old owner
  // before attach new owner" regardless of worker ordering.
  private audioRingProducerOwner: AudioRingWorkerRole | null = null;
  private micRingConsumerOwner: AudioRingWorkerRole | null = null;
  // Explicit forwarding policies to avoid accidental multi-producer/multi-consumer bugs
  // as real devices move between workers (e.g. HDA in the IO worker).
  //
  // When unset (null), these resolve to a mode-specific default:
  // - Demo mode (`activeDiskImage == null`): cpu owns both rings (tone/loopback demos).
  // - VM mode   (`activeDiskImage != null`): io owns both rings (real devices live in IO worker).
  private audioRingBufferOwnerOverride: RingBufferOwner | null = null;
  private micRingBufferOwnerOverride: RingBufferOwner | null = null;

  private cursorImage: { width: number; height: number; rgba8: ArrayBuffer } | null = null;
  private cursorState: { enabled: boolean; x: number; y: number; hotX: number; hotY: number } | null = null;

  private netTraceEnabled = false;
  private nextNetTraceRequestId = 1;
  private pendingNetTraceRequests = new Map<number, PendingNetTraceRequest>();
  private pendingNetTraceStatusRequests = new Map<number, PendingNetTraceStatusRequest>();

  private activeConfig: AeroConfig | null = null;
  private configVersion = 0;
  private workerConfigAckVersions: Partial<Record<WorkerRole, number>> = {};

  private vmState: VmLifecycleState = "stopped";
  private lastFatal: WorkerCoordinatorFatalDetail | null = null;
  private lastNonFatal: WorkerCoordinatorNonFatalDetail | null = null;

  private readonly fullRestartBackoff = new RestartBackoff(500, 30_000);
  private readonly workerRestartBackoff: Record<WorkerRole, RestartBackoff> = {
    cpu: new RestartBackoff(250, 10_000),
    gpu: new RestartBackoff(250, 10_000),
    io: new RestartBackoff(250, 10_000),
    jit: new RestartBackoff(250, 10_000),
    net: new RestartBackoff(250, 10_000),
  };

  private pendingFullRestartTimer: number | null = null;
  private pendingWorkerRestartTimers: Partial<Record<WorkerRole, number>> = {};
  private pendingFullRestart:
    | {
        atMs: number;
        delayMs: number;
        reason: string;
        attempt: number;
      }
    | null = null;

  addEventListener<K extends keyof WorkerCoordinatorEventMap>(
    type: K,
    listener: (event: CustomEvent<WorkerCoordinatorEventMap[K]>) => void,
    options?: boolean | AddEventListenerOptions,
  ): void {
    this.events.addEventListener(type, listener as unknown as EventListener, options);
  }

  removeEventListener<K extends keyof WorkerCoordinatorEventMap>(
    type: K,
    listener: (event: CustomEvent<WorkerCoordinatorEventMap[K]>) => void,
    options?: boolean | EventListenerOptions,
  ): void {
    this.events.removeEventListener(type, listener as unknown as EventListener, options);
  }

  checkSupport(): { ok: boolean; reason?: string } {
    return checkSharedMemorySupport();
  }

  getVmState(): VmLifecycleState {
    return this.vmState;
  }

  getLastFatalEvent(): WorkerCoordinatorFatalDetail | null {
    return this.lastFatal;
  }

  getLastNonFatalEvent(): WorkerCoordinatorNonFatalDetail | null {
    return this.lastNonFatal;
  }

  getPendingFullRestart():
    | {
        atMs: number;
        delayMs: number;
        reason: string;
        attempt: number;
      }
    | null {
    return this.pendingFullRestart;
  }

  start(config: AeroConfig, opts?: { platformFeatures?: PlatformFeatureReport }): void {
    if (this.shared) return;

    this.cancelPendingRestarts();
    this.activeConfig = config;
    if (opts?.platformFeatures) {
      this.platformFeatures = opts.platformFeatures;
    }

    // VM mode (`activeDiskImage != null`) uses Aero's boot-critical synchronous Rust
    // disk/controller stack (aero-storage::VirtualDisk + AHCI/IDE). That stack
    // requires OPFS `FileSystemSyncAccessHandle` / `createSyncAccessHandle()`.
    // IndexedDB is async-only and cannot safely substitute.
    if (config.activeDiskImage !== null) {
      const features = this.platformFeatures ?? detectPlatformFeatures();
      this.platformFeatures = features;
      if (!features.opfsSyncAccessHandle) {
        throw new Error(
          "Cannot start VM with a disk image: OPFS SyncAccessHandle (FileSystemFileHandle.createSyncAccessHandle) is unavailable. Aero's boot-critical Rust AHCI/IDE controller path requires synchronous disk I/O; IndexedDB cannot be used as a drop-in fallback. Use a Chromium-based browser with SyncAccessHandle support, or run the demo mode without a disk image.",
        );
      }
    }

    if (!config.enableWorkers) {
      throw new Error("Workers are disabled by configuration.");
    }

    const support = this.checkSupport();
    if (!support.ok) {
      throw new Error(support.reason ?? "Shared memory unsupported");
    }

    this.setVmState("starting", "start");

    try {
      const segments = allocateSharedMemorySegments({ guestRamMiB: config.guestMemoryMiB });
      const shared = createSharedMemoryViews(segments);
      this.shared = shared;
      this.runId += 1;
      const runId = this.runId;
      this.nextCmdSeq = 1;
      this.workerConfigAckVersions = {};
      this.serialOutputText = "";
      this.serialOutputBytes = 0;
      this.resetRequestCount = 0;
      this.lastResetRequestAtMs = 0;
      this.wasmStatus = {};
      this.lastHeartbeatFromRing = 0;
      this.cursorImage = null;
      this.cursorState = null;

      // Dedicated, tiny SharedArrayBuffer for GPU frame scheduling state/metrics.
      // Keeping it separate from the main control region avoids growing the core IPC layout.
      this.frameStateSab = new SharedArrayBuffer(8 * Int32Array.BYTES_PER_ELEMENT);

      const perfChannel = maybeGetHudPerfChannel();

      for (const role of WORKER_ROLES) {
        this.spawnWorker(role, segments);
      }

      // If the UI attached audio/mic rings before the workers were started, forward them now
      // using the current policy (otherwise we would wait until READY).
      this.syncMicrophoneRingBufferAttachments();
      this.syncAudioRingBufferAttachments();

      this.broadcastConfig(config);
      for (const role of WORKER_ROLES) {
        void this.eventLoop(role, runId);
      }

      void this.postWorkerInitMessages({ runId, segments, perfChannel });
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      this.recordFatal({
        kind: "start_failed",
        message,
        stack: err instanceof Error ? err.stack : undefined,
        atMs: nowMs(),
      });
      this.stopWorkersInternal({ clearShared: true });
      this.setVmState("failed", "start_failed");
      throw err;
    }
  }

  updateConfig(config: AeroConfig): void {
    if (this.activeConfig && aeroConfigsEqual(this.activeConfig, config)) {
      return;
    }
    this.activeConfig = config;

    // It is possible for callers to start the coordinator in demo mode (no disk) and
    // then later toggle into VM mode via `updateConfig` (for example, if the static
    // config file loads after workers have already been started).
    //
    // VM mode requires OPFS SyncAccessHandle; do not allow an implicit fallback to
    // async backends (e.g. IndexedDB) for the synchronous Rust disk/controller path.
    if (config.activeDiskImage !== null && config.enableWorkers) {
      const features = this.platformFeatures ?? detectPlatformFeatures();
      this.platformFeatures = features;
      if (!features.opfsSyncAccessHandle) {
        const message =
          "Cannot start VM with a disk image: OPFS SyncAccessHandle (FileSystemFileHandle.createSyncAccessHandle) is unavailable. Aero's boot-critical Rust AHCI/IDE controller path requires synchronous disk I/O; IndexedDB cannot be used as a drop-in fallback. Use a Chromium-based browser with SyncAccessHandle support, or run the demo mode without a disk image.";
        if (this.shared) {
          this.recordFatal({ kind: "start_failed", message, atMs: nowMs() });
          this.cancelPendingRestarts();
          this.stopWorkersInternal({ clearShared: true });
          this.setVmState("failed", "opfs_sync_access_handle_required");
        }
        return;
      }
    }

    if (!this.shared) {
      return;
    }

    if (!config.enableWorkers) {
      this.stop();
      return;
    }

    const desiredLayout = computeGuestRamLayout(config.guestMemoryMiB * 1024 * 1024);
    if (this.shared.guestLayout.guest_size !== desiredLayout.guest_size) {
      this.restart();
      return;
    }

    this.broadcastConfig(config);

    // `activeDiskImage` toggles whether we're in demo vs VM mode; when no explicit ring
    // owner override is set, recompute the default forwarding targets.
    this.syncMicrophoneRingBufferAttachments();
    this.syncAudioRingBufferAttachments();
  }

  stop(): void {
    this.cancelPendingRestarts();
    this.fullRestartBackoff.reset();
    for (const role of WORKER_ROLES) {
      this.workerRestartBackoff[role].reset();
    }
    this.stopWorkersInternal({ clearShared: true });
    this.setVmState("stopped", "stop");
  }

  powerOff(): void {
    this.cancelPendingRestarts();
    this.fullRestartBackoff.reset();
    for (const role of WORKER_ROLES) {
      this.workerRestartBackoff[role].reset();
    }
    this.stopWorkersInternal({ clearShared: true });
    this.setVmState("poweredOff", "poweroff");
  }

  restart(): void {
    const config = this.activeConfig;
    if (!config) {
      throw new Error("Cannot restart without an active config.");
    }

    this.cancelPendingRestarts();
    this.fullRestartBackoff.reset();
    for (const role of WORKER_ROLES) {
      this.workerRestartBackoff[role].reset();
    }

    this.setVmState("restarting", "restart");
    this.stopWorkersInternal({ clearShared: true });

    try {
      this.start(config);
    } catch (err) {
      console.error(err);
    }
  }

  /**
   * Attempt to restart a single worker in-place.
   *
   * Note: `gpu` and `net` are treated as restartable without tearing down the entire
   * VM; other workers share stop flags and guest state, so we fall back to a full
   * restart.
   */
  restartWorker(role: WorkerRole): void {
    if (role !== "gpu" && role !== "net") {
      this.restart();
      return;
    }

    this.cancelPendingWorkerRestart(role);
    this.workerRestartBackoff[role].reset();
    this.requestWorkerRestart(role, { reason: "restartWorker", useBackoff: false });
  }

  reset(reason = "reset"): void {
    const shared = this.shared;
    const config = this.activeConfig;
    if (!shared || !config) return;
    if (!config.enableWorkers) return;
    if (this.vmState === "resetting") return;

    this.cancelPendingRestarts();
    this.setVmState("resetting", reason);

    // Tear down workers but keep shared memory segments so guest RAM can be preserved.
    this.stopWorkersInternal({ clearShared: false });

    this.resetSharedStatus(shared);
    this.resetAllRings(shared.segments.control);
    // Reset the CPU↔I/O AIPC rings so the restarted workers don't observe stale
    // device-bus traffic from the previous run.
    shared.segments.ioIpc = createIoIpcSab();
    if (this.frameStateSab) new Int32Array(this.frameStateSab).fill(0);

    this.nextCmdSeq = 1;
    this.workerConfigAckVersions = {};
    this.wasmStatus = {};
    this.lastHeartbeatFromRing = 0;
    this.cursorImage = null;
    this.cursorState = null;

    const runId = this.runId;
    const perfChannel = maybeGetHudPerfChannel();
    for (const role of WORKER_ROLES) {
      this.spawnWorker(role, shared.segments);
    }
    // Preserve ring attachments across reset (if any) while still enforcing ownership policy.
    this.syncMicrophoneRingBufferAttachments();
    this.syncAudioRingBufferAttachments();
    this.broadcastConfig(config);
    for (const role of WORKER_ROLES) {
      void this.eventLoop(role, runId);
    }
    void this.postWorkerInitMessages({ runId, segments: shared.segments, perfChannel });
  }

  getWorker(role: WorkerRole): Worker | undefined {
    return this.workers[role]?.worker;
  }

  getFrameStateSab(): SharedArrayBuffer | undefined {
    return this.frameStateSab;
  }

  getConfigVersion(): number {
    return this.configVersion;
  }

  getWorkerConfigAckVersions(): Record<WorkerRole, number> {
    return {
      cpu: this.workerConfigAckVersions.cpu ?? 0,
      gpu: this.workerConfigAckVersions.gpu ?? 0,
      io: this.workerConfigAckVersions.io ?? 0,
      jit: this.workerConfigAckVersions.jit ?? 0,
      net: this.workerConfigAckVersions.net ?? 0,
    };
  }

  getWorkerStatuses(): Record<WorkerRole, WorkerStatus> {
    return {
      cpu: this.workers.cpu?.status ?? { state: "stopped" },
      gpu: this.workers.gpu?.status ?? { state: "stopped" },
      io: this.workers.io?.status ?? { state: "stopped" },
      jit: this.workers.jit?.status ?? { state: "stopped" },
      net: this.workers.net?.status ?? { state: "stopped" },
    };
  }

  getWorkerWasmStatus(role: WorkerRole): WorkerWasmStatus | undefined {
    return this.wasmStatus[role];
  }

  getHeartbeatCounter(): number {
    if (!this.shared) return 0;
    return Atomics.load(this.shared.status, StatusIndex.HeartbeatCounter);
  }

  getLastHeartbeatFromRing(): number {
    return this.lastHeartbeatFromRing;
  }

  getSerialOutputText(): string {
    return this.serialOutputText;
  }

  getSerialOutputBytes(): number {
    return this.serialOutputBytes;
  }

  getCpuIrqBitmapLo(): number {
    if (!this.shared) return 0;
    return Atomics.load(this.shared.status, StatusIndex.CpuIrqBitmapLo) >>> 0;
  }

  getCpuIrqBitmapHi(): number {
    if (!this.shared) return 0;
    return Atomics.load(this.shared.status, StatusIndex.CpuIrqBitmapHi) >>> 0;
  }

  getCpuA20Enabled(): boolean {
    if (!this.shared) return false;
    return Atomics.load(this.shared.status, StatusIndex.CpuA20Enabled) !== 0;
  }

  getResetRequestCount(): number {
    return this.resetRequestCount;
  }

  getLastResetRequestAtMs(): number {
    return this.lastResetRequestAtMs;
  }

  getGuestCounter0(): number {
    if (!this.shared) return 0;
    return Atomics.load(this.shared.guestI32, CPU_WORKER_DEMO_GUEST_COUNTER_INDEX);
  }

  getIoInputBatchCounter(): number {
    if (!this.shared) return 0;
    return Atomics.load(this.shared.status, StatusIndex.IoInputBatchCounter);
  }

  getIoInputEventCounter(): number {
    if (!this.shared) return 0;
    return Atomics.load(this.shared.status, StatusIndex.IoInputEventCounter);
  }

  getAudioProducerBufferLevelFrames(): number {
    if (!this.shared) return 0;
    return Atomics.load(this.shared.status, StatusIndex.AudioBufferLevelFrames) >>> 0;
  }

  getAudioProducerUnderrunCount(): number {
    if (!this.shared) return 0;
    return Atomics.load(this.shared.status, StatusIndex.AudioUnderrunCount) >>> 0;
  }

  getAudioProducerOverrunCount(): number {
    if (!this.shared) return 0;
    return Atomics.load(this.shared.status, StatusIndex.AudioOverrunCount) >>> 0;
  }

  getIoWorker(): Worker | null {
    return this.workers.io?.worker ?? null;
  }

  getCpuWorker(): Worker | null {
    return this.workers.cpu?.worker ?? null;
  }

  getIoIpcSab(): SharedArrayBuffer | null {
    return this.shared?.segments.ioIpc ?? null;
  }

  getStatusView(): Int32Array | null {
    return this.shared?.status ?? null;
  }

  getVgaFramebuffer(): SharedArrayBuffer | null {
    return this.shared?.vgaFramebuffer ?? null;
  }

  getSharedFramebuffer(): { sab: SharedArrayBuffer; offsetBytes: number } | null {
    const shared = this.shared;
    if (!shared) return null;
    return { sab: shared.sharedFramebuffer, offsetBytes: shared.sharedFramebufferOffsetBytes };
  }

  getGuestMemory(): WebAssembly.Memory | null {
    return this.shared?.segments.guestMemory ?? null;
  }

  setAudioRingBufferOwner(owner: RingBufferOwner): void {
    if (owner === "both") {
      throw new Error("Audio ring buffer owner 'both' violates SPSC constraints; choose 'cpu', 'io', or 'none'.");
    }
    this.audioRingBufferOwnerOverride = owner;
    this.syncAudioRingBufferAttachments();
  }

  setMicrophoneRingBufferOwner(owner: RingBufferOwner): void {
    if (owner === "both") {
      throw new Error("Microphone ring buffer owner 'both' violates SPSC constraints; choose 'cpu', 'io', or 'none'.");
    }
    this.micRingBufferOwnerOverride = owner;
    this.syncMicrophoneRingBufferAttachments();
  }

  setMicrophoneRingBuffer(ringBuffer: SharedArrayBuffer | null, sampleRate: number): void {
    if (ringBuffer !== null) {
      const Sab = globalThis.SharedArrayBuffer;
      if (typeof Sab === "undefined") {
        throw new Error("SharedArrayBuffer is unavailable; microphone capture requires crossOriginIsolated.");
      }
      if (!(ringBuffer instanceof Sab)) {
        throw new Error("setMicrophoneRingBuffer expects a SharedArrayBuffer or null.");
      }
    }

    this.micRingBuffer = ringBuffer;
    this.micSampleRate = (sampleRate ?? 0) | 0;

    this.syncMicrophoneRingBufferAttachments();
  }

  setAudioRingBuffer(
    ringBuffer: SharedArrayBuffer | null,
    capacityFrames: number,
    channelCount: number,
    dstSampleRate: number,
  ): void {
    if (ringBuffer !== null) {
      const Sab = globalThis.SharedArrayBuffer;
      if (typeof Sab === "undefined") {
        throw new Error("SharedArrayBuffer is unavailable; audio output requires crossOriginIsolated.");
      }
      if (!(ringBuffer instanceof Sab)) {
        throw new Error("setAudioRingBuffer expects a SharedArrayBuffer or null.");
      }
    }

    this.audioRingBuffer = ringBuffer;
    this.audioCapacityFrames = capacityFrames >>> 0;
    this.audioChannelCount = channelCount >>> 0;
    this.audioDstSampleRate = dstSampleRate >>> 0;

    this.syncAudioRingBufferAttachments();
  }

  /**
   * Backwards-compatible alias for attaching an AudioWorklet output ring buffer.
   *
   * The newer runtime API uses `setAudioRingBuffer(...)` with explicit dstSampleRate;
   * some callers (and older docs) refer to this as the "audio output ring buffer".
   */
  setAudioOutputRingBuffer(
    ringBuffer: SharedArrayBuffer | null,
    sampleRate: number,
    channelCount: number,
    capacityFrames: number,
  ): void {
    this.setAudioRingBuffer(ringBuffer, capacityFrames, channelCount, sampleRate);
  }

  setNetTraceEnabled(enabled: boolean): void {
    this.netTraceEnabled = !!enabled;
    const net = this.workers.net?.worker;
    if (!net) return;
    this.syncNetTraceEnabledToWorker(net);
  }

  isNetTraceEnabled(): boolean {
    return this.netTraceEnabled;
  }

  clearNetTrace(): void {
    const net = this.workers.net?.worker;
    if (!net) return;
    net.postMessage({ kind: "net.trace.clear" } satisfies NetTraceClearMessage);
  }

  getNetTraceStats(timeoutMs = 1000): Promise<NetTraceStatusResponseMessage> {
    const net = this.workers.net?.worker;
    if (!net) {
      return Promise.reject(new Error("Cannot get network trace stats: net worker is not running."));
    }

    const requestId = this.nextNetTraceRequestId++;
    return new Promise<NetTraceStatusResponseMessage>((resolve, reject) => {
      const timer = globalThis.setTimeout(() => {
        this.pendingNetTraceStatusRequests.delete(requestId);
        reject(new Error(`Timed out waiting for net trace stats (requestId=${requestId})`));
      }, timeoutMs);
      (timer as unknown as { unref?: () => void }).unref?.();

      const pending: PendingNetTraceStatusRequest = {
        resolve,
        reject: reject as (err: Error) => void,
        timeout: timer as unknown as number,
      };
      this.pendingNetTraceStatusRequests.set(requestId, pending);

      try {
        net.postMessage({ kind: "net.trace.status", requestId } satisfies NetTraceStatusMessage);
      } catch (err) {
        clearTimeout(pending.timeout);
        this.pendingNetTraceStatusRequests.delete(requestId);
        reject(err instanceof Error ? err : new Error(String(err)));
      }
    });
  }

  takeNetTracePcapng(timeoutMs = 10_000): Promise<Uint8Array<ArrayBuffer>> {
    const net = this.workers.net?.worker;
    if (!net) {
      return Promise.reject(new Error("Cannot take network trace: net worker is not running."));
    }

    const requestId = this.nextNetTraceRequestId++;
    return new Promise<Uint8Array<ArrayBuffer>>((resolve, reject) => {
      const timer = globalThis.setTimeout(() => {
        this.pendingNetTraceRequests.delete(requestId);
        reject(new Error(`Timed out waiting for net trace capture (requestId=${requestId})`));
      }, timeoutMs);
      (timer as unknown as { unref?: () => void }).unref?.();

      const pending: PendingNetTraceRequest = {
        resolve,
        reject: reject as (err: Error) => void,
        timeout: timer as unknown as number,
      };
      this.pendingNetTraceRequests.set(requestId, pending);

      try {
        net.postMessage({ kind: "net.trace.take_pcapng", requestId } satisfies NetTraceTakePcapngMessage);
      } catch (err) {
        clearTimeout(pending.timeout);
        this.pendingNetTraceRequests.delete(requestId);
        reject(err instanceof Error ? err : new Error(String(err)));
      }
    });
  }

  exportNetTracePcapng(timeoutMs = 10_000): Promise<Uint8Array<ArrayBuffer>> {
    const net = this.workers.net?.worker;
    if (!net) {
      return Promise.reject(new Error("Cannot export network trace: net worker is not running."));
    }

    const requestId = this.nextNetTraceRequestId++;
    return new Promise<Uint8Array<ArrayBuffer>>((resolve, reject) => {
      const timer = globalThis.setTimeout(() => {
        this.pendingNetTraceRequests.delete(requestId);
        reject(new Error(`Timed out waiting for net trace capture (requestId=${requestId})`));
      }, timeoutMs);
      (timer as unknown as { unref?: () => void }).unref?.();

      const pending: PendingNetTraceRequest = {
        resolve,
        reject: reject as (err: Error) => void,
        timeout: timer as unknown as number,
      };
      this.pendingNetTraceRequests.set(requestId, pending);

      try {
        net.postMessage({ kind: "net.trace.export_pcapng", requestId } satisfies NetTraceExportPcapngMessage);
      } catch (err) {
        clearTimeout(pending.timeout);
        this.pendingNetTraceRequests.delete(requestId);
        reject(err instanceof Error ? err : new Error(String(err)));
      }
    });
  }

  private defaultAudioRingBufferOwner(): RingBufferOwner {
    // Demo mode (no disk): the CPU worker runs the tone/loopback demos.
    // VM mode (disk present): audio devices live in the IO worker.
    return this.activeConfig?.activeDiskImage ? "io" : "cpu";
  }

  private effectiveAudioRingBufferOwner(): RingBufferOwner {
    return this.audioRingBufferOwnerOverride ?? this.defaultAudioRingBufferOwner();
  }

  private defaultMicrophoneRingBufferOwner(): RingBufferOwner {
    // Demo mode: loopback demo consumes mic samples in CPU worker.
    // VM mode: microphone is consumed by the IO worker device model.
    return this.activeConfig?.activeDiskImage ? "io" : "cpu";
  }

  private effectiveMicrophoneRingBufferOwner(): RingBufferOwner {
    return this.micRingBufferOwnerOverride ?? this.defaultMicrophoneRingBufferOwner();
  }

  private syncAudioRingBufferAttachments(): void {
    const ringBuffer = this.audioRingBuffer;
    const owner = this.effectiveAudioRingBufferOwner();

    if (owner === "both") {
      throw new Error("Audio ring buffer owner 'both' violates SPSC constraints; choose 'cpu', 'io', or 'none'.");
    }

    let nextOwner: AudioRingWorkerRole | null = null;
    if (ringBuffer && (owner === "cpu" || owner === "io")) {
      nextOwner = owner;
    }

    const prevOwner = this.audioRingProducerOwner;
    if (prevOwner && prevOwner !== nextOwner) {
      const info = this.workers[prevOwner];
      if (info) {
        info.worker.postMessage({
          type: "setAudioRingBuffer",
          ringBuffer: null,
          capacityFrames: 0,
          channelCount: 0,
          dstSampleRate: 0,
        } satisfies SetAudioRingBufferMessage);
      }
    }

    for (const role of AUDIO_RING_WORKER_ROLES) {
      const info = this.workers[role];
      if (!info) continue;
      const attach = role === nextOwner;
      info.worker.postMessage({
        type: "setAudioRingBuffer",
        ringBuffer: attach ? ringBuffer : null,
        capacityFrames: attach ? this.audioCapacityFrames : 0,
        channelCount: attach ? this.audioChannelCount : 0,
        dstSampleRate: attach ? this.audioDstSampleRate : 0,
      } satisfies SetAudioRingBufferMessage);
    }

    this.audioRingProducerOwner = nextOwner;
  }

  private syncMicrophoneRingBufferAttachments(): void {
    const ringBuffer = this.micRingBuffer;
    const owner = this.effectiveMicrophoneRingBufferOwner();

    if (owner === "both") {
      throw new Error(
        "Microphone ring buffer owner 'both' violates SPSC constraints; choose 'cpu', 'io', or 'none'.",
      );
    }

    let nextOwner: AudioRingWorkerRole | null = null;
    if (ringBuffer && (owner === "cpu" || owner === "io")) {
      nextOwner = owner;
    }

    const prevOwner = this.micRingConsumerOwner;
    if (prevOwner && prevOwner !== nextOwner) {
      const info = this.workers[prevOwner];
      if (info) {
        info.worker.postMessage({
          type: "setMicrophoneRingBuffer",
          ringBuffer: null,
          sampleRate: 0,
        } satisfies SetMicrophoneRingBufferMessage);
      }
    }

    for (const role of AUDIO_RING_WORKER_ROLES) {
      const info = this.workers[role];
      if (!info) continue;
      const attach = role === nextOwner;
      info.worker.postMessage({
        type: "setMicrophoneRingBuffer",
        ringBuffer: attach ? ringBuffer : null,
        sampleRate: attach ? this.micSampleRate : 0,
      } satisfies SetMicrophoneRingBufferMessage);
    }

    this.micRingConsumerOwner = nextOwner;
  }

  async snapshotSaveToOpfs(path: string): Promise<void> {
    if (this.snapshotInFlight) {
      throw new Error("VM snapshot already in progress.");
    }

    const cpu = this.workers.cpu;
    const io = this.workers.io;
    const net = this.workers.net;
    if (!cpu?.worker || !io?.worker || !net?.worker) {
      throw new Error("Cannot save VM snapshot: CPU/IO/NET workers are not running.");
    }
    if (cpu.status.state !== "ready" || io.status.state !== "ready" || net.status.state !== "ready") {
      throw new Error("Cannot save VM snapshot: CPU/IO/NET workers are not ready.");
    }

    this.snapshotInFlight = true;
    try {
      await this.pauseWorkersForSnapshot({ cpu: cpu.worker, io: io.worker, net: net.worker });

      const cpuState = await this.snapshotRpc<VmSnapshotCpuStateMessage>(
        cpu.worker,
        { kind: "vm.snapshot.getCpuState" },
        "vm.snapshot.cpuState",
        { timeoutMs: 10_000 },
      );
      this.assertSnapshotOk("getCpuState", cpuState);
      if (!cpuState.ok) {
        // Unreachable due to assert above, but helps TS narrow.
        throw new Error("cpuState missing payload");
      }

      // Forward CPU state bytes to the IO worker so it can build an `aero-snapshot`
      // container alongside shared guest RAM + device blobs.
      const cpuBuf = cpuState.cpu;
      const mmuBuf = cpuState.mmu;
      const saved = await this.snapshotRpc<VmSnapshotSavedMessage>(
        io.worker,
        { kind: "vm.snapshot.saveToOpfs", path, cpu: cpuBuf, mmu: mmuBuf },
        "vm.snapshot.saved",
        { timeoutMs: 120_000, transfer: [cpuBuf, mmuBuf] },
      );
      this.assertSnapshotOk("saveToOpfs", saved);
    } finally {
      try {
        await this.resumeWorkersAfterSnapshot();
      } finally {
        this.snapshotInFlight = false;
      }
    }
  }

  async snapshotRestoreFromOpfs(path: string): Promise<void> {
    if (this.snapshotInFlight) {
      throw new Error("VM snapshot already in progress.");
    }

    const cpu = this.workers.cpu;
    const io = this.workers.io;
    const net = this.workers.net;
    if (!cpu?.worker || !io?.worker || !net?.worker) {
      throw new Error("Cannot restore VM snapshot: CPU/IO/NET workers are not running.");
    }
    if (cpu.status.state !== "ready" || io.status.state !== "ready" || net.status.state !== "ready") {
      throw new Error("Cannot restore VM snapshot: CPU/IO/NET workers are not ready.");
    }

    this.snapshotInFlight = true;
    try {
      await this.pauseWorkersForSnapshot({ cpu: cpu.worker, io: io.worker, net: net.worker });

      const restored = await this.snapshotRpc<VmSnapshotRestoredMessage>(
        io.worker,
        { kind: "vm.snapshot.restoreFromOpfs", path },
        "vm.snapshot.restored",
        { timeoutMs: 120_000 },
      );
      this.assertSnapshotOk("restoreFromOpfs", restored);
      if (!restored.ok) {
        throw new Error("restored missing payload");
      }

      const cpuBuf = restored.cpu;
      const mmuBuf = restored.mmu;
      const cpuSet = await this.snapshotRpc<VmSnapshotCpuStateSetMessage>(
        cpu.worker,
        { kind: "vm.snapshot.setCpuState", cpu: cpuBuf, mmu: mmuBuf },
        "vm.snapshot.cpuStateSet",
        { timeoutMs: 10_000, transfer: [cpuBuf, mmuBuf] },
      );
      this.assertSnapshotOk("setCpuState", cpuSet);
    } finally {
      try {
        await this.resumeWorkersAfterSnapshot();
      } finally {
        this.snapshotInFlight = false;
      }
    }
  }

  /**
   * Pause CPU → IO → NET, then clear the NET_TX/NET_RX rings.
   *
   * Ordering matters:
   * - NET_TX/NET_RX are shared-memory rings accessed by multiple workers (guest + host sides).
   * - Resetting/draining them while any worker can still enqueue/dequeue is racy and can
   *   leave stale Ethernet frames visible after snapshot restore.
   * - Therefore, we pause the "guest" side first (CPU then IO), then pause NET, and only
   *   once *all* participants are paused do we reset the rings.
   */
  private async pauseWorkersForSnapshot(opts: { cpu: Worker; io: Worker; net: Worker }): Promise<void> {
    // NOTE: Pausing sequentially enforces the stronger ordering required to safely reset
    // the NET rings without races from CPU/IO enqueue/dequeue.
    const cpuPause = await this.snapshotRpc<VmSnapshotPausedMessage>(opts.cpu, { kind: "vm.snapshot.pause" }, "vm.snapshot.paused", {
      timeoutMs: 5_000,
    });
    this.assertSnapshotOk("pause cpu", cpuPause);

    const ioPause = await this.snapshotRpc<VmSnapshotPausedMessage>(opts.io, { kind: "vm.snapshot.pause" }, "vm.snapshot.paused", {
      timeoutMs: 5_000,
    });
    this.assertSnapshotOk("pause io", ioPause);

    const netPause = await this.snapshotRpc<VmSnapshotPausedMessage>(opts.net, { kind: "vm.snapshot.pause" }, "vm.snapshot.paused", {
      timeoutMs: 5_000,
    });
    this.assertSnapshotOk("pause net", netPause);

    // Now that CPU+IO+NET are all paused, it is safe to reset the NET_TX/NET_RX rings.
    this.resetNetRingsForSnapshot();
  }

  private resetNetRingsForSnapshot(): void {
    const shared = this.shared;
    if (!shared) {
      throw new Error("Cannot reset NET rings for snapshot: shared memory is not initialized.");
    }

    // NET_TX/NET_RX rings live in the shared `ioIpc` region and are not included in
    // snapshot files. Reset them at snapshot boundaries so stale frames don't leak
    // into restored VM state.
    //
    // NOTE: `RingBuffer.reset()` is only safe when there are no concurrent producers
    // or consumers. `pauseWorkersForSnapshot()` enforces that invariant.
    const ioIpc = shared.segments.ioIpc;
    try {
      openRingByKind(ioIpc, IO_IPC_NET_TX_QUEUE_KIND).reset();
      openRingByKind(ioIpc, IO_IPC_NET_RX_QUEUE_KIND).reset();
    } catch (err) {
      console.warn("[coordinator] Failed to reset NET_TX/NET_RX rings during snapshot:", err);
    }
  }
  private assertSnapshotOk(context: string, msg: { ok: boolean; error?: VmSnapshotSerializedError; kind?: unknown }): void {
    if (msg.ok) return;
    const err = msg.error;
    const suffix = err ? `${err.name}: ${err.message}` : "unknown error";
    throw new Error(`Snapshot ${context} failed: ${suffix}`);
  }

  private async resumeWorkersAfterSnapshot(): Promise<void> {
    const cpu = this.workers.cpu?.worker;
    const io = this.workers.io?.worker;
    if (!cpu || !io) return;
    const netInfo = this.workers.net;
    const net = netInfo?.status.state === "ready" ? netInfo.worker : undefined;

    const cpuResume = this.snapshotRpc<VmSnapshotResumedMessage>(cpu, { kind: "vm.snapshot.resume" }, "vm.snapshot.resumed", {
      timeoutMs: 5_000,
    });
    const ioResume = this.snapshotRpc<VmSnapshotResumedMessage>(io, { kind: "vm.snapshot.resume" }, "vm.snapshot.resumed", {
      timeoutMs: 5_000,
    });

    // Best-effort: resume even if one worker fails to respond; we don't want a
    // snapshot error to strand a running VM forever.
    await Promise.allSettled([cpuResume, ioResume]);

    // Resume net after the guest/device side is back up (CPU + IO).
    if (!net) return;
    const netResume = this.snapshotRpc<VmSnapshotResumedMessage>(net, { kind: "vm.snapshot.resume" }, "vm.snapshot.resumed", {
      timeoutMs: 5_000,
    });
    await Promise.allSettled([netResume]);
  }

  private snapshotRpc<TResponse extends { kind: string; requestId: number }>(
    worker: Worker,
    request: Record<string, unknown>,
    expectedKind: TResponse["kind"],
    opts: { timeoutMs: number; transfer?: Transferable[] },
  ): Promise<TResponse> {
    const requestId = this.nextSnapshotRequestId++;
    const msg = { ...request, requestId };

    return new Promise<TResponse>((resolve, reject) => {
      const onMessage = (ev: MessageEvent<unknown>) => {
        const data = ev.data as { kind?: unknown; requestId?: unknown };
        if (!data || typeof data !== "object") return;
        if (data.kind !== expectedKind) return;
        if (data.requestId !== requestId) return;
        cleanup();
        resolve(ev.data as TResponse);
      };

      const cleanup = () => {
        worker.removeEventListener("message", onMessage as EventListener);
        clearTimeout(timer);
      };

      const timer = setTimeout(() => {
        cleanup();
        reject(new Error(`Timed out waiting for ${expectedKind} (requestId=${requestId})`));
      }, opts.timeoutMs);

      worker.addEventListener("message", onMessage as EventListener);
      try {
        if (opts.transfer && opts.transfer.length) {
          worker.postMessage(msg, opts.transfer);
        } else {
          worker.postMessage(msg);
        }
      } catch (err) {
        cleanup();
        reject(err instanceof Error ? err : new Error(String(err)));
      }
    });
  }

  private emitEvent<K extends keyof WorkerCoordinatorEventMap>(type: K, detail: WorkerCoordinatorEventMap[K]): void {
    this.events.dispatchEvent(new CustomEvent(type, { detail }));
  }

  private setVmState(next: VmLifecycleState, reason?: string): void {
    const prev = this.vmState;
    if (prev === next) return;
    this.vmState = next;

    const atMs = nowMs();
    this.emitEvent("statechange", { prev, next, reason, atMs });

    if (perf.traceEnabled) {
      if (next === "resetting") perf.instant("vm:reset", "p", { reason });
      else if (next === "poweredOff") perf.instant("vm:poweroff", "p", { reason });
      else if (next === "restarting") perf.instant("vm:restart", "p", { reason });
      else if (next === "starting") perf.instant("vm:start", "p", { reason });
      else if (next === "running") perf.instant("vm:running", "p", { reason });
      else if (next === "failed") perf.instant("vm:failed", "p", { reason });
      else if (next === "stopped") perf.instant("vm:stopped", "p", { reason });
    }
  }

  private recordFatal(detail: WorkerCoordinatorFatalDetail): void {
    this.lastFatal = detail;
    this.emitEvent("fatal", detail);
    if (perf.traceEnabled) perf.instant("vm:fatal", "p", { kind: detail.kind, role: detail.role ?? "unknown" });
  }

  private recordNonFatal(detail: WorkerCoordinatorNonFatalDetail): void {
    this.lastNonFatal = detail;
    this.emitEvent("nonfatal", detail);
  }

  private cancelPendingWorkerRestart(role: WorkerRole): void {
    const timer = this.pendingWorkerRestartTimers[role];
    if (timer !== undefined) {
      clearTimeout(timer);
      delete this.pendingWorkerRestartTimers[role];
    }
  }

  private cancelPendingRestarts(): void {
    if (this.pendingFullRestartTimer !== null) {
      clearTimeout(this.pendingFullRestartTimer);
      this.pendingFullRestartTimer = null;
      this.pendingFullRestart = null;
    }
    for (const role of WORKER_ROLES) {
      this.cancelPendingWorkerRestart(role);
    }
  }

  private resetSharedStatus(shared: SharedMemoryViews): void {
    const layout = shared.guestLayout;
    shared.status.fill(0);
    Atomics.store(shared.status, StatusIndex.GuestBase, layout.guest_base | 0);
    Atomics.store(shared.status, StatusIndex.GuestSize, layout.guest_size | 0);
    Atomics.store(shared.status, StatusIndex.RuntimeReserved, layout.runtime_reserved | 0);
  }

  private resetAllRings(control: SharedArrayBuffer): void {
    for (const role of WORKER_ROLES) {
      const regions = ringRegionsForWorker(role);
      this.resetRing(control, regions.command.byteOffset);
      this.resetRing(control, regions.event.byteOffset);
    }
  }

  private resetRing(control: SharedArrayBuffer, offsetBytes: number): void {
    const ctrl = new Int32Array(control, offsetBytes, ringCtrl.WORDS);
    const cap = Atomics.load(ctrl, ringCtrl.CAPACITY);
    Atomics.store(ctrl, ringCtrl.HEAD, 0);
    Atomics.store(ctrl, ringCtrl.TAIL_RESERVE, 0);
    Atomics.store(ctrl, ringCtrl.TAIL_COMMIT, 0);
    Atomics.store(ctrl, ringCtrl.CAPACITY, cap);
    Atomics.notify(ctrl, ringCtrl.HEAD, 1);
    Atomics.notify(ctrl, ringCtrl.TAIL_COMMIT, 1);
  }

  private stopWorkersInternal(options: { clearShared: boolean }): void {
    const shared = this.shared;
    if (!shared) return;

    this.runId += 1;
    Atomics.store(shared.status, StatusIndex.StopRequested, 1);

    for (const role of WORKER_ROLES) {
      const info = this.workers[role];
      if (!info) continue;
      if (role === "net") {
        const err = new Error("Net worker stopped while a trace request was pending.");
        this.rejectAllPendingNetTraceRequests(err);
        this.rejectAllPendingNetTraceStatusRequests(err);
      }
      void this.trySendCommand(info, { kind: "shutdown" });
      info.worker.terminate();
      info.status = { state: "stopped" };
      setReadyFlag(shared.status, role, false);
    }

    this.workers = {};
    this.wasmStatus = {};
    this.workerConfigAckVersions = {};

    if (options.clearShared) {
      this.shared = undefined;
      this.frameStateSab = undefined;
      this.lastHeartbeatFromRing = 0;
      this.nextCmdSeq = 1;
    }
  }

  private scheduleFullRestart(reason: string): void {
    const cfg = this.activeConfig;
    if (!cfg?.enableWorkers) {
      this.setVmState("failed", reason);
      return;
    }
    if (this.pendingFullRestartTimer !== null) return;

    const delayMs = this.fullRestartBackoff.nextDelayMs();
    const attempt = this.fullRestartBackoff.getAttemptCount();
    const atMs = nowMs() + delayMs;
    this.pendingFullRestart = { atMs, delayMs, reason, attempt };
    if (perf.traceEnabled) perf.instant("vm:restart:schedule", "p", { reason, delayMs, attempt });

    this.setVmState("restarting", reason);
    this.stopWorkersInternal({ clearShared: true });

    const fullRestartTimer = globalThis.setTimeout(() => {
      this.pendingFullRestartTimer = null;
      this.pendingFullRestart = null;
      const latest = this.activeConfig;
      if (!latest?.enableWorkers) {
        this.setVmState("stopped", "restart_cancelled");
        return;
      }
      try {
        this.start(latest);
      } catch (err) {
        console.error(err);
      }
    }, delayMs);
    (fullRestartTimer as unknown as { unref?: () => void }).unref?.();
    this.pendingFullRestartTimer = fullRestartTimer as unknown as number;
  }

  private requestWorkerRestart(role: WorkerRole, opts: { reason: string; useBackoff: boolean }): void {
    const shared = this.shared;
    const cfg = this.activeConfig;
    if (!shared || !cfg?.enableWorkers) return;
    if (this.pendingFullRestartTimer !== null) return;
    if (this.pendingWorkerRestartTimers[role] !== undefined) return;

    const delayMs = opts.useBackoff ? this.workerRestartBackoff[role].nextDelayMs() : 0;
    const attempt = this.workerRestartBackoff[role].getAttemptCount();
    if (perf.traceEnabled) perf.instant("vm:worker:restart:schedule", "p", { role, reason: opts.reason, delayMs, attempt });

    this.terminateWorker(role);
    if (this.vmState === "running") {
      this.setVmState("starting", `worker_restart:${role}`);
    }

    const workerRestartTimer = globalThis.setTimeout(() => {
      delete this.pendingWorkerRestartTimers[role];
      if (!this.shared) return;
      const latestConfig = this.activeConfig;
      if (!latestConfig?.enableWorkers) return;

      if (perf.traceEnabled) perf.instant("vm:worker:restart", "p", { role, reason: opts.reason });

      this.resetRing(this.shared.segments.control, ringRegionsForWorker(role).command.byteOffset);
      this.resetRing(this.shared.segments.control, ringRegionsForWorker(role).event.byteOffset);

      const runId = this.runId;
      const perfChannel = maybeGetHudPerfChannel();
      this.spawnWorker(role, this.shared.segments);
      this.sendConfigToWorker(role, this.configVersion, latestConfig);
      void this.postWorkerInitMessages({ runId, segments: this.shared.segments, perfChannel, roles: [role] });
    }, delayMs);
    (workerRestartTimer as unknown as { unref?: () => void }).unref?.();
    this.pendingWorkerRestartTimers[role] = workerRestartTimer as unknown as number;
  }

  private terminateWorker(role: WorkerRole): void {
    const shared = this.shared;
    const info = this.workers[role];
    if (!shared || !info) return;

    if (role === "net") {
      const err = new Error("Net worker restarted while a trace request was pending.");
      this.rejectAllPendingNetTraceRequests(err);
      this.rejectAllPendingNetTraceStatusRequests(err);
    }

    setReadyFlag(shared.status, role, false);
    info.worker.terminate();
    delete this.workers[role];
    delete this.wasmStatus[role];
    this.workerConfigAckVersions[role] = 0;
    info.eventRing.waitForDataAsync(0).catch(() => {});
  }

  private spawnWorker(role: WorkerRole, segments: SharedMemoryViews["segments"]): void {
    const shared = this.shared;
    if (!shared) return;

    const regions = ringRegionsForWorker(role);
    const commandRing = new RingBuffer(segments.control, regions.command.byteOffset);
    const eventRing = new RingBuffer(segments.control, regions.event.byteOffset);

    let worker: Worker;
    switch (role) {
      case "cpu":
        worker = new Worker(new URL("../workers/cpu.worker.ts", import.meta.url), { type: "module" });
        break;
      case "gpu":
        worker = new Worker(new URL("../workers/gpu.worker.ts", import.meta.url), { type: "module" });
        break;
      case "io":
        worker = new Worker(new URL("../workers/io.worker.ts", import.meta.url), { type: "module" });
        break;
      case "jit":
        worker = new Worker(new URL("../workers/jit.worker.ts", import.meta.url), { type: "module" });
        break;
      case "net":
        worker = new Worker(new URL("../workers/net.worker.ts", import.meta.url), { type: "module" });
        break;
      default: {
        const neverRole: never = role;
        throw new Error(`Unknown worker role: ${String(neverRole)}`);
      }
    }

    perf.registerWorker(worker, { threadName: role });
    if (perf.traceEnabled) perf.instant("boot:worker:spawn", "p", { role });

    const instanceId = this.nextWorkerInstanceId++;
    const info: WorkerInfo = { role, instanceId, worker, status: { state: "starting" }, commandRing, eventRing };
    this.workers[role] = info;

    worker.onmessage = (ev) => this.onWorkerMessage(role, instanceId, ev.data);
    worker.onerror = (ev) => this.onWorkerScriptError(role, instanceId, ev);
    worker.onmessageerror = () => this.onWorkerMessageError(role, instanceId);

    setReadyFlag(shared.status, role, false);
  }

  private sendConfigToWorker(role: WorkerRole, version: number, config: AeroConfig): void {
    const info = this.workers[role];
    if (!info) return;

    this.workerConfigAckVersions[role] = 0;
    const msg: ConfigUpdateMessage = { kind: "config.update", version, config, platformFeatures: this.platformFeatures ?? undefined };
    info.worker.postMessage(msg);
  }

  private broadcastConfig(config: AeroConfig): void {
    this.configVersion += 1;
    const version = this.configVersion;
    for (const role of WORKER_ROLES) {
      this.sendConfigToWorker(role, version, config);
    }
  }

  private async postWorkerInitMessages(opts: {
    runId: number;
    segments: SharedMemoryViews["segments"];
    perfChannel: PerfChannel | null;
    roles?: WorkerRole[];
  }): Promise<void> {
    const { runId, segments, perfChannel } = opts;
    const roles = opts.roles ?? WORKER_ROLES;

    const tryVariantOrder: WasmVariant[] = ["threaded", "single"];
    let precompiled: { variant: WasmVariant; module: WebAssembly.Module } | null = null;

    for (const variant of tryVariantOrder) {
      try {
        const compiled = await precompileWasm(variant);
        if (!isWasmModuleCloneable(compiled.module)) {
          console.warn(
            "[wasm] WebAssembly.Module is not structured-cloneable in this environment; falling back to per-worker compilation.",
          );
          precompiled = null;
          break;
        }
        precompiled = { variant, module: compiled.module };
        break;
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        console.warn(`[wasm] Precompile (${variant}) failed; falling back. Error: ${message}`);
      }
    }

    if (!this.shared || this.runId !== runId) return;

    let moduleToSend: WebAssembly.Module | undefined = precompiled?.module;
    let variantToSend: WasmVariant | undefined = precompiled?.variant;

    for (const role of roles) {
      const info = this.workers[role];
      if (!info) continue;

      const baseInit: WorkerInitMessage = {
        kind: "init",
        role,
        controlSab: segments.control,
        guestMemory: segments.guestMemory,
        vgaFramebuffer: segments.vgaFramebuffer,
        ioIpcSab: segments.ioIpc,
        sharedFramebuffer: segments.sharedFramebuffer,
        sharedFramebufferOffsetBytes: segments.sharedFramebufferOffsetBytes,
        frameStateSab: this.frameStateSab,
        platformFeatures: this.platformFeatures ?? undefined,
      };

      if (perfChannel) {
        const workerKind = workerRoleToPerfWorkerKind(role);
        const buffer = perfChannel.buffers[workerKind];
        if (perfChannel.frameHeader instanceof SharedArrayBuffer && buffer instanceof SharedArrayBuffer) {
          baseInit.perfChannel = {
            runStartEpochMs: perfChannel.runStartEpochMs,
            frameHeader: perfChannel.frameHeader,
            buffer,
            workerKind,
          };
        }
      }

      try {
        if (moduleToSend) {
          info.worker.postMessage({ ...baseInit, wasmModule: moduleToSend, wasmVariant: variantToSend });
        } else {
          info.worker.postMessage(baseInit);
        }
      } catch (err) {
        const msg = err instanceof Error ? err.message : String(err);
        console.warn(`[wasm] Failed to send precompiled module to worker (${role}); falling back. Error: ${msg}`);
        moduleToSend = undefined;
        variantToSend = undefined;
        info.worker.postMessage(baseInit);
      }
    }
  }

  private onWorkerMessage(role: WorkerRole, instanceId: number, data: unknown): void {
    const info = this.workers[role];
    const shared = this.shared;
    if (!info || !shared) return;
    if (info.instanceId !== instanceId) return;

    const maybeAck = data as Partial<ConfigAckMessage>;
    if (maybeAck?.kind === "config.ack" && typeof maybeAck.version === "number") {
      this.workerConfigAckVersions[role] = maybeAck.version;
      return;
    }

    const maybeNetTracePcapng = data as Partial<NetTracePcapngMessage>;
    if (
      maybeNetTracePcapng?.kind === "net.trace.pcapng" &&
      typeof maybeNetTracePcapng.requestId === "number" &&
      maybeNetTracePcapng.bytes instanceof ArrayBuffer
    ) {
      const pending = this.pendingNetTraceRequests.get(maybeNetTracePcapng.requestId);
      if (!pending) return;
      clearTimeout(pending.timeout);
      this.pendingNetTraceRequests.delete(maybeNetTracePcapng.requestId);
      pending.resolve(new Uint8Array(maybeNetTracePcapng.bytes) as Uint8Array<ArrayBuffer>);
      return;
    }

    const maybeNetTraceStatus = data as Partial<NetTraceStatusResponseMessage>;
    if (
      maybeNetTraceStatus?.kind === "net.trace.status" &&
      typeof maybeNetTraceStatus.requestId === "number" &&
      typeof maybeNetTraceStatus.enabled === "boolean" &&
      typeof maybeNetTraceStatus.records === "number" &&
      typeof maybeNetTraceStatus.bytes === "number"
    ) {
      const pending = this.pendingNetTraceStatusRequests.get(maybeNetTraceStatus.requestId);
      if (!pending) return;
      clearTimeout(pending.timeout);
      this.pendingNetTraceStatusRequests.delete(maybeNetTraceStatus.requestId);
      pending.resolve(maybeNetTraceStatus as NetTraceStatusResponseMessage);
      return;
    }

    const maybeCursorImage = data as Partial<CursorSetImageMessage>;
    if (
      maybeCursorImage?.kind === "cursor.set_image" &&
      typeof maybeCursorImage.width === "number" &&
      typeof maybeCursorImage.height === "number" &&
      maybeCursorImage.rgba8 instanceof ArrayBuffer
    ) {
      this.setCursorImage(maybeCursorImage.width, maybeCursorImage.height, maybeCursorImage.rgba8);
      return;
    }

    const maybeCursorState = data as Partial<CursorSetStateMessage>;
    if (
      maybeCursorState?.kind === "cursor.set_state" &&
      typeof maybeCursorState.enabled === "boolean" &&
      typeof maybeCursorState.x === "number" &&
      typeof maybeCursorState.y === "number" &&
      typeof maybeCursorState.hotX === "number" &&
      typeof maybeCursorState.hotY === "number"
    ) {
      this.setCursorState(
        maybeCursorState.enabled,
        maybeCursorState.x,
        maybeCursorState.y,
        maybeCursorState.hotX,
        maybeCursorState.hotY,
      );
      return;
    }

    if (role === "gpu") {
      if (isGpuWorkerGpuErrorMessage(data)) {
        const err = data.error as { message?: unknown; stack?: unknown } | undefined;
        const msgText = typeof err?.message === "string" ? err.message : "GPU error";
        const stackText = typeof err?.stack === "string" ? err.stack : undefined;
        if (data.fatal) {
          info.status = { state: "failed", error: msgText };
          setReadyFlag(shared.status, role, false);
          this.recordFatal({ kind: "gpu_fatal", role, message: msgText, stack: stackText, atMs: nowMs() });
          this.scheduleFullRestart("gpu_fatal");
        } else {
          this.recordNonFatal({ kind: "gpu_error", role, message: msgText, stack: stackText, atMs: nowMs() });
        }
        return;
      }

      if (isGpuWorkerErrorEventMessage(data)) {
        const evt = (data as GpuWorkerErrorEventMessage).event as { category?: unknown; message?: unknown };
        const category = typeof evt.category === "string" ? evt.category : "";
        const msgText = typeof evt.message === "string" ? evt.message : "GPU event";
        if (category === "DeviceLost") {
          this.recordNonFatal({ kind: "gpu_device_lost", role, message: msgText, atMs: nowMs() });
          this.requestWorkerRestart("gpu", { reason: "gpu_device_lost", useBackoff: true });
        }
        return;
      }
    }
    // Workers use structured `postMessage` for low-rate control/status messages
    // (READY/ERROR/WASM_READY). High-frequency device/bus events flow through
    // the AIPC command/event rings (`web/src/ipc/*`).
    const msg = data as Partial<ProtocolMessage>;
    if (msg?.type === MessageType.READY) {
      info.status = { state: "ready" };
      setReadyFlag(shared.status, role, true);
      this.workerRestartBackoff[role].reset();

      if (role === "net") {
        this.syncNetTraceEnabledToWorker(info.worker);
      }
      // Forward optional audio/mic ring buffers using the current ownership policy.
      // This is re-sent on READY so newly restarted workers inherit any existing attachments.
      this.syncMicrophoneRingBufferAttachments();
      this.syncAudioRingBufferAttachments();

      // Kick the worker to start its minimal demo loop.
      void this.trySendCommand(info, { kind: "nop", seq: this.nextCmdSeq++ });

      if (role === "gpu") {
        this.flushCursorToGpuWorker();
      }

      this.maybeMarkRunning();
      return;
    }

    if (msg?.type === MessageType.WASM_READY) {
      const wasmMsg = msg as Partial<WasmReadyMessage>;
      if ((wasmMsg.variant === "single" || wasmMsg.variant === "threaded") && typeof wasmMsg.value === "number") {
        this.wasmStatus[role] = { variant: wasmMsg.variant, value: wasmMsg.value };
      }
      return;
    }

    if (msg?.type === MessageType.ERROR && typeof (msg as { message?: unknown }).message === "string") {
      const message = (msg as { message: string }).message;
      info.status = { state: "failed", error: message };
      setReadyFlag(shared.status, role, false);

      if (role === "gpu") {
        const lower = message.toLowerCase();
        const kind: WorkerCoordinatorNonFatalKind =
          lower.includes("context lost") || lower.includes("device lost") ? "gpu_device_lost" : "gpu_error";
        this.recordNonFatal({ kind, role, message, atMs: nowMs() });
        this.requestWorkerRestart("gpu", { reason: kind, useBackoff: true });
        return;
      }

      if (role === "net") {
        this.recordNonFatal({ kind: "net_error", role, message, atMs: nowMs() });
        this.requestWorkerRestart("net", { reason: "net_error", useBackoff: true });
        return;
      }

      this.recordFatal({ kind: "worker_reported_error", role, message, atMs: nowMs() });
      this.scheduleFullRestart("worker_reported_error");
    }
  }

  private onWorkerScriptError(role: WorkerRole, instanceId: number, ev: ErrorEvent): void {
    const shared = this.shared;
    const info = this.workers[role];
    if (!shared || !info) return;
    if (info.instanceId !== instanceId) return;

    const formatted = formatWorkerError(ev);
    info.status = { state: "failed", error: formatted.message };
    setReadyFlag(shared.status, role, false);
    this.recordFatal({ kind: "worker_error", role, message: formatted.message, stack: formatted.stack, atMs: nowMs() });

    if (role === "gpu" || role === "net") {
      this.requestWorkerRestart(role, { reason: "worker_error", useBackoff: true });
    } else {
      this.scheduleFullRestart("worker_error");
    }
  }

  private onWorkerMessageError(role: WorkerRole, instanceId: number): void {
    const shared = this.shared;
    const info = this.workers[role];
    if (!shared || !info) return;
    if (info.instanceId !== instanceId) return;

    const message = "worker message deserialization failed";
    info.status = { state: "failed", error: message };
    setReadyFlag(shared.status, role, false);
    this.recordFatal({ kind: "worker_message_error", role, message, atMs: nowMs() });

    if (role === "gpu" || role === "net") {
      this.requestWorkerRestart(role, { reason: "worker_message_error", useBackoff: true });
    } else {
      this.scheduleFullRestart("worker_message_error");
    }
  }

  private syncNetTraceEnabledToWorker(worker: Worker): void {
    if (this.netTraceEnabled) {
      worker.postMessage({ kind: "net.trace.enable" } satisfies NetTraceEnableMessage);
    } else {
      worker.postMessage({ kind: "net.trace.disable" } satisfies NetTraceDisableMessage);
    }
  }

  private rejectAllPendingNetTraceRequests(error: Error): void {
    if (this.pendingNetTraceRequests.size > 0) {
      for (const [requestId, pending] of this.pendingNetTraceRequests) {
        clearTimeout(pending.timeout);
        pending.reject(new Error(`${error.message} (requestId=${requestId})`));
      }
      this.pendingNetTraceRequests.clear();
    }

    if (this.pendingNetTraceStatusRequests.size > 0) {
      for (const [requestId, pending] of this.pendingNetTraceStatusRequests) {
        clearTimeout(pending.timeout);
        pending.reject(new Error(`${error.message} (requestId=${requestId})`));
      }
      this.pendingNetTraceStatusRequests.clear();
    }
  }

  private rejectAllPendingNetTraceStatusRequests(error: Error): void {
    if (this.pendingNetTraceStatusRequests.size === 0) return;
    for (const [requestId, pending] of this.pendingNetTraceStatusRequests) {
      clearTimeout(pending.timeout);
      pending.reject(new Error(`${error.message} (requestId=${requestId})`));
    }
    this.pendingNetTraceStatusRequests.clear();
  }

  private maybeMarkRunning(): void {
    if (this.vmState === "poweredOff" || this.vmState === "stopped" || this.vmState === "failed") {
      return;
    }

    for (const role of WORKER_ROLES) {
      if (this.workers[role]?.status.state !== "ready") {
        return;
      }
    }

    this.fullRestartBackoff.reset();
    this.setVmState("running", "all_ready");
  }

  private drainEventRing(info: WorkerInfo): void {
    while (true) {
      const payload = info.eventRing.tryPop();
      if (!payload) break;

      let evt: Event;
      try {
        evt = decodeEvent(payload);
      } catch (err) {
        console.error(`[${info.role}] Failed to decode event`, err);
        continue;
      }
      this.handleEvent(info, evt);
    }
  }

  private async eventLoop(role: WorkerRole, runId: number): Promise<void> {
    while (this.shared && this.runId === runId) {
      const info = this.workers[role];
      if (!info) {
        await new Promise((resolve) => setTimeout(resolve, 50));
        continue;
      }

      this.drainEventRing(info);

      if (!this.shared || this.runId !== runId) return;
      await info.eventRing.waitForDataAsync(1000);
    }
  }

  private handleEvent(info: WorkerInfo, evt: Event): void {
    const shared = this.shared;
    if (!shared) return;

    switch (evt.kind) {
      case "ack":
        this.lastHeartbeatFromRing = evt.seq;
        return;
      case "serialOutput": {
        this.serialOutputBytes += evt.data.byteLength;
        const text = this.serialDecoder.decode(evt.data);
        this.serialOutputText += text;
        const maxChars = 16 * 1024;
        if (this.serialOutputText.length > maxChars) {
          this.serialOutputText = this.serialOutputText.slice(this.serialOutputText.length - maxChars);
        }

        const portStr = `0x${(evt.port >>> 0).toString(16)}`;
        // eslint-disable-next-line no-console
        console.log(`[serial ${portStr}] ${text}`);
        return;
      }
      case "log": {
        const prefix = `[${info.role}]`;
        switch (evt.level) {
          case "trace":
            console.debug(prefix, evt.message);
            break;
          case "debug":
            console.debug(prefix, evt.message);
            break;
          case "info":
            console.info(prefix, evt.message);
            break;
          case "warn":
            console.warn(prefix, evt.message);
            break;
          case "error":
            console.error(prefix, evt.message);
            break;
        }
        if (evt.level === "warn" || evt.level === "error") {
          this.recordNonFatal({ kind: "ipc_log", role: info.role, message: `${evt.level}: ${evt.message}`, atMs: nowMs() });
        }
        return;
      }
      case "resetRequest":
        this.resetRequestCount += 1;
        this.lastResetRequestAtMs = nowMs();
        if (perf.traceEnabled) perf.instant("vm:reset:request", "p", { role: info.role });
        this.reset("resetRequest");
        return;
      case "tripleFault":
        this.recordFatal({ kind: "ipc_triple_fault", role: info.role, message: "Triple fault", atMs: nowMs() });
        this.reset("tripleFault");
        return;
      case "panic":
        info.status = { state: "failed", error: evt.message };
        setReadyFlag(shared.status, info.role, false);
        this.recordFatal({ kind: "ipc_panic", role: info.role, message: evt.message, atMs: nowMs() });
        this.scheduleFullRestart("ipc_panic");
        return;
      default:
        return;
    }
  }

  private trySendCommand(info: WorkerInfo, cmd: Command): boolean {
    return info.commandRing.tryPush(encodeCommand(cmd));
  }

  private setCursorImage(width: number, height: number, rgba8: ArrayBuffer): void {
    const w = Math.max(0, width | 0);
    const h = Math.max(0, height | 0);
    if (w === 0 || h === 0) return;
    if (rgba8.byteLength < w * h * 4) return;
    this.cursorImage = { width: w, height: h, rgba8 };
    this.flushCursorToGpuWorker();
  }

  private setCursorState(enabled: boolean, x: number, y: number, hotX: number, hotY: number): void {
    this.cursorState = {
      enabled: !!enabled,
      x: x | 0,
      y: y | 0,
      hotX: Math.max(0, hotX | 0),
      hotY: Math.max(0, hotY | 0),
    };
    this.flushCursorToGpuWorker();
  }

  private flushCursorToGpuWorker(): void {
    const gpu = this.workers.gpu?.worker;
    if (!gpu) return;

    const img = this.cursorImage;
    if (img) {
      const msg: GpuRuntimeCursorSetImageMessage = {
        ...GPU_MESSAGE_BASE,
        type: "cursor_set_image",
        width: img.width,
        height: img.height,
        rgba8: img.rgba8,
      };
      gpu.postMessage(msg);
    }

    const state = this.cursorState;
    if (state) {
      const msg: GpuRuntimeCursorSetStateMessage = {
        ...GPU_MESSAGE_BASE,
        type: "cursor_set_state",
        enabled: state.enabled,
        x: state.x,
        y: state.y,
        hotX: state.hotX,
        hotY: state.hotY,
      };
      gpu.postMessage(msg);
    }
  }
}

function isWasmModuleCloneable(module: WebAssembly.Module): boolean {
  try {
    if (typeof structuredClone === "function") {
      structuredClone(module);
      return true;
    }
  } catch {
    // Fall through to MessageChannel test below.
  }

  try {
    const channel = new MessageChannel();
    channel.port1.postMessage(module);
    channel.port1.close();
    channel.port2.close();
    return true;
  } catch {
    return false;
  }
}

function aeroConfigsEqual(a: AeroConfig, b: AeroConfig): boolean {
  return (
    a.guestMemoryMiB === b.guestMemoryMiB &&
    a.enableWorkers === b.enableWorkers &&
    a.enableWebGPU === b.enableWebGPU &&
    a.proxyUrl === b.proxyUrl &&
    a.activeDiskImage === b.activeDiskImage &&
    a.logLevel === b.logLevel &&
    a.uiScale === b.uiScale
  );
}
