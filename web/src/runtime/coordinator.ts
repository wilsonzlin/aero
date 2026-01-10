import { RingBuffer } from "./ring_buffer";
import { perf } from "../perf/perf";
import {
  WORKER_ROLES,
  type WorkerRole,
  StatusIndex,
  allocateSharedMemorySegments,
  checkSharedMemorySupport,
  createSharedMemoryViews,
  type GuestRamMiB,
  ringRegionsForWorker,
  setReadyFlag,
  type SharedMemoryViews,
} from "./shared_layout";
import {
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
  version: number;
  sum: number;
}

interface WorkerInfo {
  role: WorkerRole;
  worker: Worker;
  status: WorkerStatus;
  commandRing: RingBuffer;
  eventRing: RingBuffer;
}

export class WorkerCoordinator {
  private shared?: SharedMemoryViews;
  private workers: Partial<Record<WorkerRole, WorkerInfo>> = {};
  private runId = 0;

  private lastHeartbeatFromRing = 0;
  private wasmStatus: Partial<Record<WorkerRole, WorkerWasmStatus>> = {};

  checkSupport(): { ok: boolean; reason?: string } {
    return checkSharedMemorySupport();
  }

  start(options?: { guestRamMiB?: GuestRamMiB }): void {
    if (this.shared) return;

    const support = this.checkSupport();
    if (!support.ok) {
      throw new Error(support.reason ?? "Shared memory unsupported");
    }

    const segments = allocateSharedMemorySegments({ guestRamMiB: options?.guestRamMiB });
    const shared = createSharedMemoryViews(segments);
    shared.status.fill(0);
    this.shared = shared;
    this.runId += 1;
    const runId = this.runId;

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
      perf.instant("boot:worker:spawn", "p", { role });

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
      };
      worker.postMessage(initMessage);
    }
    for (const role of WORKER_ROLES) {
      void this.eventLoop(role, runId);
    }
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

  getVgaFramebuffer(): SharedArrayBuffer | null {
    return this.shared?.vgaFramebuffer ?? null;
  }

  private onWorkerMessage(role: WorkerRole, data: unknown): void {
    const info = this.workers[role];
    const shared = this.shared;
    if (!info || !shared) return;

    // Workers currently use structured `postMessage` for READY/ERROR only.
    const msg = data as Partial<ProtocolMessage>;
    if (msg?.type === MessageType.READY) {
      info.status = { state: "ready" };
      setReadyFlag(shared.status, role, true);

      // Kick the worker to start its minimal demo loop.
      info.commandRing.push(encodeProtocolMessage({ type: MessageType.START }));
      info.commandRing.notifyData();
      return;
    }

    if (msg?.type === MessageType.WASM_READY) {
      const wasmMsg = msg as Partial<WasmReadyMessage>;
      if (
        (wasmMsg.variant === "single" || wasmMsg.variant === "threaded") &&
        typeof wasmMsg.version === "number" &&
        typeof wasmMsg.sum === "number"
      ) {
        this.wasmStatus[role] = {
          variant: wasmMsg.variant,
          version: wasmMsg.version,
          sum: wasmMsg.sum,
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
