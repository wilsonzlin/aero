import type { AeroConfig } from "../config/aero_config";
import { RingBuffer } from "./ring_buffer";
import { perf } from "../perf/perf";
import type { PlatformFeatureReport } from "../platform/features";
import { WorkerKind } from "../perf/record.js";
import type { PerfChannel } from "../perf/shared.js";
import {
  WORKER_ROLES,
  type WorkerRole,
  StatusIndex,
  allocateSharedMemorySegments,
  checkSharedMemorySupport,
  createSharedMemoryViews,
  ringRegionsForWorker,
  setReadyFlag,
  type SharedMemoryViews,
} from "./shared_layout";
import {
  type ConfigAckMessage,
  type ConfigUpdateMessage,
  MessageType,
  type ProtocolMessage,
  type WorkerInitMessage,
  type WasmReadyMessage,
  decodeProtocolMessage,
  encodeProtocolMessage,
} from "./protocol";
import type { WasmVariant } from "./wasm_context";

export type WorkerState = "starting" | "ready" | "failed" | "stopped";

export interface WorkerStatus {
  state: WorkerState;
  error?: string;
}

export interface WorkerWasmStatus {
  variant: WasmVariant;
  value: number;
}

interface WorkerInfo {
  role: WorkerRole;
  worker: Worker;
  status: WorkerStatus;
  commandRing: RingBuffer;
  eventRing: RingBuffer;
}

function maybeGetHudPerfChannel(): PerfChannel | null {
  const aero = (globalThis as unknown as { aero?: unknown }).aero as { perf?: unknown } | undefined;
  const perfApi = aero?.perf as { getChannel?: () => PerfChannel } | undefined;
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
    default: {
      const neverRole: never = role;
      throw new Error(`Unknown worker role: ${String(neverRole)}`);
    }
  }
}

export class WorkerCoordinator {
  private shared?: SharedMemoryViews;
  private workers: Partial<Record<WorkerRole, WorkerInfo>> = {};
  private runId = 0;
  private frameStateSab?: SharedArrayBuffer;
  private platformFeatures: PlatformFeatureReport | null = null;

  private lastHeartbeatFromRing = 0;
  private wasmStatus: Partial<Record<WorkerRole, WorkerWasmStatus>> = {};

  // Optional SharedArrayBuffer-backed microphone ring buffer attachment. This
  // is set by the UI and forwarded to the I/O worker when available.
  private micRingBuffer: SharedArrayBuffer | null = null;
  private micSampleRate = 0;
  private activeConfig: AeroConfig | null = null;
  private configVersion = 0;
  private workerConfigAckVersions: Partial<Record<WorkerRole, number>> = {};

  checkSupport(): { ok: boolean; reason?: string } {
    return checkSharedMemorySupport();
  }

  start(config: AeroConfig, opts?: { platformFeatures?: PlatformFeatureReport }): void {
    if (this.shared) return;

    this.activeConfig = config;
    if (opts?.platformFeatures) {
      this.platformFeatures = opts.platformFeatures;
    }
    if (!config.enableWorkers) {
      throw new Error("Workers are disabled by configuration.");
    }

    const support = this.checkSupport();
    if (!support.ok) {
      throw new Error(support.reason ?? "Shared memory unsupported");
    }

    const segments = allocateSharedMemorySegments({ guestRamMiB: config.guestMemoryMiB });
    const shared = createSharedMemoryViews(segments);
    shared.status.fill(0);
    this.shared = shared;
    this.runId += 1;
    const runId = this.runId;
    this.workerConfigAckVersions = {};
    // Dedicated, tiny SharedArrayBuffer for GPU frame scheduling state/metrics.
    // Keeping it separate from the main control region avoids growing the core IPC layout.
    this.frameStateSab = new SharedArrayBuffer(8 * Int32Array.BYTES_PER_ELEMENT);

    const perfChannel = maybeGetHudPerfChannel();

    for (const role of WORKER_ROLES) {
      const regions = ringRegionsForWorker(role);
      const commandRing = new RingBuffer(segments.control, regions.command.byteOffset, regions.command.byteLength);
      const eventRing = new RingBuffer(segments.control, regions.event.byteOffset, regions.event.byteLength);
      commandRing.reset();
      eventRing.reset();

      // IMPORTANT: Keep the `new Worker(new URL(..., import.meta.url), ...)` shape so Vite
      // can statically detect and bundle workers (including their own dependencies/assets).
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
        default: {
          const neverRole: never = role;
          throw new Error(`Unknown worker role: ${String(neverRole)}`);
        }
      }
      perf.registerWorker(worker, { threadName: role });
      if (perf.traceEnabled) perf.instant("boot:worker:spawn", "p", { role });

      const info: WorkerInfo = {
        role,
        worker,
        status: { state: "starting" },
        commandRing,
        eventRing,
      };
      this.workers[role] = info;

      worker.onmessage = (ev) => this.onWorkerMessage(role, ev.data);
      worker.onerror = (ev) => {
        info.status = { state: "failed", error: ev.message };
        setReadyFlag(shared.status, role, false);
      };
      worker.onmessageerror = () => {
        info.status = { state: "failed", error: "worker message deserialization failed" };
        setReadyFlag(shared.status, role, false);
      };

      const initMessage: WorkerInitMessage = {
        kind: "init",
        role,
        controlSab: segments.control,
        guestMemory: segments.guestMemory,
        vgaFramebuffer: segments.vgaFramebuffer,
        frameStateSab: this.frameStateSab,
        platformFeatures: this.platformFeatures ?? undefined,
      };

      if (perfChannel) {
        const workerKind = workerRoleToPerfWorkerKind(role);
        const buffer = perfChannel.buffers[workerKind];
        if (perfChannel.frameHeader instanceof SharedArrayBuffer && buffer instanceof SharedArrayBuffer) {
          initMessage.perfChannel = {
            runStartEpochMs: perfChannel.runStartEpochMs,
            frameHeader: perfChannel.frameHeader,
            buffer,
            workerKind,
          };
        }
      }
      worker.postMessage(initMessage);
    }

    this.broadcastConfig(config);
    for (const role of WORKER_ROLES) {
      void this.eventLoop(role, runId);
    }
  }

  updateConfig(config: AeroConfig): void {
    if (this.activeConfig && aeroConfigsEqual(this.activeConfig, config)) {
      return;
    }
    this.activeConfig = config;

    if (!this.shared) {
      return;
    }

    if (!config.enableWorkers) {
      this.stop();
      return;
    }

    const currentMiB = Math.round(this.shared.segments.guestMemory.buffer.byteLength / (1024 * 1024));
    if (currentMiB !== config.guestMemoryMiB) {
      this.stop();
      try {
        this.start(config);
      } catch (err) {
        console.error(err);
      }
      return;
    }

    this.broadcastConfig(config);
  }

  stop(): void {
    const shared = this.shared;
    if (!shared) return;

    // Cancel any in-flight async loops, then wake waiters so they can exit.
    this.runId += 1;
    Atomics.store(shared.status, StatusIndex.StopRequested, 1);

    for (const role of WORKER_ROLES) {
      const info = this.workers[role];
      if (!info) continue;
      info.commandRing.push(encodeProtocolMessage({ type: MessageType.STOP }));
      info.commandRing.notifyData();
      info.eventRing.notifyData();
      info.worker.terminate();
      info.status = { state: "stopped" };
      setReadyFlag(shared.status, role, false);
    }

    this.workers = {};
    this.shared = undefined;
    this.wasmStatus = {};
    this.frameStateSab = undefined;
    this.workerConfigAckVersions = {};
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
    };
  }

  getWorkerStatuses(): Record<WorkerRole, WorkerStatus> {
    return {
      cpu: this.workers.cpu?.status ?? { state: "stopped" },
      gpu: this.workers.gpu?.status ?? { state: "stopped" },
      io: this.workers.io?.status ?? { state: "stopped" },
      jit: this.workers.jit?.status ?? { state: "stopped" },
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

  getGuestCounter0(): number {
    if (!this.shared) return 0;
    return Atomics.load(this.shared.guestI32, 0);
  }

  getIoInputBatchCounter(): number {
    if (!this.shared) return 0;
    return Atomics.load(this.shared.status, StatusIndex.IoInputBatchCounter);
  }

  getIoInputEventCounter(): number {
    if (!this.shared) return 0;
    return Atomics.load(this.shared.status, StatusIndex.IoInputEventCounter);
  }

  getIoWorker(): Worker | null {
    return this.workers.io?.worker ?? null;
  }

  getVgaFramebuffer(): SharedArrayBuffer | null {
    return this.shared?.vgaFramebuffer ?? null;
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

    const info = this.workers.io;
    if (info) {
      info.worker.postMessage({
        type: "setMicrophoneRingBuffer",
        ringBuffer,
        sampleRate: this.micSampleRate,
      });
    }
  }

  private broadcastConfig(config: AeroConfig): void {
    this.configVersion += 1;
    const version = this.configVersion;
    for (const role of WORKER_ROLES) {
      const info = this.workers[role];
      if (!info) continue;
      const msg: ConfigUpdateMessage = { kind: "config.update", version, config, platformFeatures: this.platformFeatures ?? undefined };
      info.worker.postMessage(msg);
    }
  }

  private onWorkerMessage(role: WorkerRole, data: unknown): void {
    const info = this.workers[role];
    const shared = this.shared;
    if (!info || !shared) return;

    const maybeAck = data as Partial<ConfigAckMessage>;
    if (maybeAck?.kind === "config.ack" && typeof maybeAck.version === "number") {
      this.workerConfigAckVersions[role] = maybeAck.version;
      return;
    }

    // Workers currently use structured `postMessage` for READY/ERROR only.
    const msg = data as Partial<ProtocolMessage>;
    if (msg?.type === MessageType.READY) {
      info.status = { state: "ready" };
      setReadyFlag(shared.status, role, true);

      if (role === "io" && this.micRingBuffer) {
        info.worker.postMessage({
          type: "setMicrophoneRingBuffer",
          ringBuffer: this.micRingBuffer,
          sampleRate: this.micSampleRate,
        });
      }

      // Kick the worker to start its minimal demo loop.
      info.commandRing.push(encodeProtocolMessage({ type: MessageType.START }));
      info.commandRing.notifyData();
      return;
    }

    if (msg?.type === MessageType.WASM_READY) {
      const wasmMsg = msg as Partial<WasmReadyMessage>;
      if (
        (wasmMsg.variant === "single" || wasmMsg.variant === "threaded") &&
        typeof wasmMsg.value === "number"
      ) {
        this.wasmStatus[role] = {
          variant: wasmMsg.variant,
          value: wasmMsg.value,
        };
      }
      return;
    }

    if (msg?.type === MessageType.ERROR && typeof (msg as { message?: unknown }).message === "string") {
      info.status = { state: "failed", error: (msg as { message: string }).message };
      setReadyFlag(shared.status, role, false);
    }
  }

  private drainEventRing(info: WorkerInfo): void {
    while (true) {
      const payload = info.eventRing.pop();
      if (!payload) break;

      const msg = decodeProtocolMessage(payload);
      if (msg?.type === MessageType.HEARTBEAT) {
        this.lastHeartbeatFromRing = msg.counter;
      }
    }
  }
  private async eventLoop(role: WorkerRole, runId: number): Promise<void> {
    while (this.shared && this.runId === runId) {
      const info = this.workers[role];
      if (!info) return;

      this.drainEventRing(info);

      if (!this.shared || this.runId !== runId) return;
      await info.eventRing.waitForDataAsync(1000);
    }
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
