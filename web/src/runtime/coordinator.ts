import { RingBuffer } from "./ring_buffer";
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
  MessageType,
  type ProtocolMessage,
  type WorkerInitMessage,
  decodeProtocolMessage,
  encodeProtocolMessage,
} from "./protocol";

export type WorkerState = "starting" | "ready" | "failed" | "stopped";

export interface WorkerStatus {
  state: WorkerState;
  error?: string;
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
  private pollIntervalId?: number;

  private lastHeartbeatFromRing = 0;

  checkSupport(): { ok: boolean; reason?: string } {
    return checkSharedMemorySupport();
  }

  start(): void {
    if (this.shared) return;

    const support = this.checkSupport();
    if (!support.ok) {
      throw new Error(support.reason ?? "Shared memory unsupported");
    }

    const segments = allocateSharedMemorySegments();
    const shared = createSharedMemoryViews(segments);
    shared.status.fill(0);
    this.shared = shared;

    for (const role of WORKER_ROLES) {
      const regions = ringRegionsForWorker(role);
      const commandRing = new RingBuffer(
        segments.control,
        regions.command.byteOffset,
        regions.command.byteLength,
      );
      const eventRing = new RingBuffer(segments.control, regions.event.byteOffset, regions.event.byteLength);
      commandRing.reset();
      eventRing.reset();

      const workerUrl =
        role === "cpu"
          ? new URL("../workers/cpu.worker.ts", import.meta.url)
          : role === "gpu"
            ? new URL("../workers/gpu.worker.ts", import.meta.url)
            : role === "io"
              ? new URL("../workers/io.worker.ts", import.meta.url)
              : new URL("../workers/jit.worker.ts", import.meta.url);

      const worker = new Worker(workerUrl, { type: "module" });

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
        guestSab: segments.guest,
      };
      worker.postMessage(initMessage);
    }

    this.pollIntervalId = globalThis.setInterval(() => this.poll(), 100) as unknown as number;
  }

  stop(): void {
    const shared = this.shared;
    if (!shared) return;

    Atomics.store(shared.status, StatusIndex.StopRequested, 1);

    if (this.pollIntervalId !== undefined) {
      clearInterval(this.pollIntervalId);
      this.pollIntervalId = undefined;
    }

    for (const role of WORKER_ROLES) {
      const info = this.workers[role];
      if (!info) continue;
      info.commandRing.push(encodeProtocolMessage({ type: MessageType.STOP }));
      info.commandRing.notifyData();
      info.worker.terminate();
      info.status = { state: "stopped" };
      setReadyFlag(shared.status, role, false);
    }

    this.workers = {};
    this.shared = undefined;
  }

  getWorkerStatuses(): Record<WorkerRole, WorkerStatus> {
    return {
      cpu: this.workers.cpu?.status ?? { state: "stopped" },
      gpu: this.workers.gpu?.status ?? { state: "stopped" },
      io: this.workers.io?.status ?? { state: "stopped" },
      jit: this.workers.jit?.status ?? { state: "stopped" },
    };
  }

  getHeartbeatCounter(): number {
    if (!this.shared) return 0;
    return Atomics.load(this.shared.status, StatusIndex.HeartbeatCounter);
  }

  getLastHeartbeatFromRing(): number {
    return this.lastHeartbeatFromRing;
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

    if (msg?.type === MessageType.ERROR && typeof (msg as { message?: unknown }).message === "string") {
      info.status = { state: "failed", error: (msg as { message: string }).message };
      setReadyFlag(shared.status, role, false);
    }
  }

  private poll(): void {
    const shared = this.shared;
    if (!shared) return;

    for (const role of WORKER_ROLES) {
      const info = this.workers[role];
      if (!info) continue;

      while (true) {
        const payload = info.eventRing.pop();
        if (!payload) break;

        const msg = decodeProtocolMessage(payload);
        if (msg?.type === MessageType.HEARTBEAT) {
          this.lastHeartbeatFromRing = msg.counter;
        }
      }
    }
  }
}

