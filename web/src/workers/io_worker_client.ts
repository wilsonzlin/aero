import type { WorkerOpenToken } from "../storage/disk_image_store";
import type { RemoteDiskCacheStatus, RemoteDiskTelemetrySnapshot } from "../platform/remote_disk";
import { createIpcBuffer, openRingByKind } from "../ipc/ipc";
import { RECORD_ALIGN, ringCtrl } from "../ipc/layout";
import { decodeEvent, encodeCommand } from "../ipc/protocol";
import type { RingBuffer as IpcRingBuffer } from "../ipc/ring_buffer";
import {
  CONTROL_BYTES,
  IO_IPC_CMD_QUEUE_KIND,
  IO_IPC_EVT_QUEUE_KIND,
  IO_IPC_RING_CAPACITY_BYTES,
  StatusIndex,
  createSharedMemoryViews,
  ringRegionsForWorker,
} from "../runtime/shared_layout";
import type { WorkerInitMessage } from "../runtime/protocol";

function initControlRing(control: SharedArrayBuffer, byteOffset: number, byteLength: number): void {
  const capacityBytes = byteLength - ringCtrl.BYTES;
  if (capacityBytes < 0) throw new Error("control ring region too small");
  if (capacityBytes % RECORD_ALIGN !== 0) {
    throw new Error(`control ring capacity must be aligned to ${RECORD_ALIGN} (got ${capacityBytes})`);
  }
  new Int32Array(control, byteOffset, ringCtrl.WORDS).set([0, 0, 0, capacityBytes]);
}

type OpenActiveDiskRequest = {
  id: number;
  type: "openActiveDisk";
  token: WorkerOpenToken;
};

type OpenActiveDiskResponse = {
  id: number;
  type: "openActiveDiskResult";
  ok: true;
  size: number;
  syncAccessHandleAvailable: boolean;
};

type OpenActiveDiskErrorResponse = {
  id: number;
  type: "openActiveDiskResult";
  ok: false;
  error: string;
};

type OpenRemoteDiskRequest = {
  id: number;
  type: "openRemoteDisk";
  url: string;
  options?: {
    blockSize?: number;
    cacheLimitMiB?: number | null;
    credentials?: RequestCredentials;
    prefetchSequentialBlocks?: number;
    cacheBackend?: "opfs" | "idb";
    cacheImageId?: string;
    cacheVersion?: string;
  };
};

type OpenRemoteDiskResponse = {
  id: number;
  type: "openRemoteDiskResult";
  ok: true;
  size: number;
};

type OpenRemoteDiskErrorResponse = {
  id: number;
  type: "openRemoteDiskResult";
  ok: false;
  error: string;
};

type GetRemoteDiskCacheStatusRequest = { id: number; type: "getRemoteDiskCacheStatus" };
type GetRemoteDiskCacheStatusResponse = {
  id: number;
  type: "getRemoteDiskCacheStatusResult";
  ok: true;
  status: RemoteDiskCacheStatus;
};
type GetRemoteDiskCacheStatusErrorResponse = {
  id: number;
  type: "getRemoteDiskCacheStatusResult";
  ok: false;
  error: string;
};

type GetRemoteDiskTelemetryRequest = { id: number; type: "getRemoteDiskTelemetry" };
type GetRemoteDiskTelemetryResponse = {
  id: number;
  type: "getRemoteDiskTelemetryResult";
  ok: true;
  telemetry: RemoteDiskTelemetrySnapshot;
};
type GetRemoteDiskTelemetryErrorResponse = {
  id: number;
  type: "getRemoteDiskTelemetryResult";
  ok: false;
  error: string;
};

type ClearRemoteDiskCacheRequest = { id: number; type: "clearRemoteDiskCache" };
type ClearRemoteDiskCacheResponse = { id: number; type: "clearRemoteDiskCacheResult"; ok: true };
type ClearRemoteDiskCacheErrorResponse = { id: number; type: "clearRemoteDiskCacheResult"; ok: false; error: string };

type FlushRemoteDiskCacheRequest = { id: number; type: "flushRemoteDiskCache" };
type FlushRemoteDiskCacheResponse = { id: number; type: "flushRemoteDiskCacheResult"; ok: true };
type FlushRemoteDiskCacheErrorResponse = { id: number; type: "flushRemoteDiskCacheResult"; ok: false; error: string };

type CloseRemoteDiskRequest = { id: number; type: "closeRemoteDisk" };
type CloseRemoteDiskResponse = { id: number; type: "closeRemoteDiskResult"; ok: true };
type CloseRemoteDiskErrorResponse = { id: number; type: "closeRemoteDiskResult"; ok: false; error: string };

type DiskReadRequest = {
  diskOffset: number;
  guestMemory: WebAssembly.Memory;
  guestOffset: number;
  length: number;
  timeoutMs?: number;
};

type WorkerResponse =
  | OpenActiveDiskResponse
  | OpenActiveDiskErrorResponse
  | OpenRemoteDiskResponse
  | OpenRemoteDiskErrorResponse
  | GetRemoteDiskCacheStatusResponse
  | GetRemoteDiskCacheStatusErrorResponse
  | GetRemoteDiskTelemetryResponse
  | GetRemoteDiskTelemetryErrorResponse
  | ClearRemoteDiskCacheResponse
  | ClearRemoteDiskCacheErrorResponse
  | FlushRemoteDiskCacheResponse
  | FlushRemoteDiskCacheErrorResponse
  | CloseRemoteDiskResponse
  | CloseRemoteDiskErrorResponse;

export class IoWorkerClient {
  readonly #worker: Worker;
  #nextId = 1;
  readonly #pending = new Map<number, { resolve: (value: any) => void; reject: (err: Error) => void }>();

  #ipcInitPromise: Promise<void> | null = null;
  #guestMemory: WebAssembly.Memory | null = null;
  #status: Int32Array | null = null;
  #ioCmdRing: IpcRingBuffer | null = null;
  #ioEvtRing: IpcRingBuffer | null = null;
  #nextIoIpcId = 1;
  #diskIoChain: Promise<void> = Promise.resolve();

  constructor() {
    this.#worker = new Worker(new URL("./io.worker.ts", import.meta.url), { type: "module" });

    this.#worker.addEventListener("message", (event: MessageEvent<WorkerResponse>) => {
      const msg = event.data;
      if (!msg || typeof msg.id !== "number") return;
      const pending = this.#pending.get(msg.id);
      if (!pending) return;
      this.#pending.delete(msg.id);

      if (msg.ok) {
        pending.resolve(msg);
      } else {
        pending.reject(new Error((msg as { error?: string }).error ?? "I/O worker request failed"));
      }
    });
  }

  close(): void {
    this.#pending.clear();
    this.#diskIoChain = Promise.resolve();
    this.#ipcInitPromise = null;
    this.#guestMemory = null;
    this.#status = null;
    this.#ioCmdRing = null;
    this.#ioEvtRing = null;
    this.#worker.terminate();
  }

  async openActiveDisk(token: WorkerOpenToken): Promise<OpenActiveDiskResponse> {
    const id = this.#nextId++;
    const req: OpenActiveDiskRequest = { id, type: "openActiveDisk", token };
    return await new Promise((resolve, reject) => {
      this.#pending.set(id, { resolve, reject });
      this.#worker.postMessage(req);
    });
  }

  async openRemoteDisk(
    url: string,
    options?: {
      blockSize?: number;
      cacheLimitMiB?: number | null;
      credentials?: RequestCredentials;
      prefetchSequentialBlocks?: number;
      cacheBackend?: "opfs" | "idb";
      cacheImageId?: string;
      cacheVersion?: string;
    },
  ): Promise<OpenRemoteDiskResponse> {
    const id = this.#nextId++;
    const req: OpenRemoteDiskRequest = { id, type: "openRemoteDisk", url, options };
    return await new Promise((resolve, reject) => {
      this.#pending.set(id, { resolve, reject });
      this.#worker.postMessage(req);
    });
  }

  async getRemoteDiskCacheStatus(): Promise<GetRemoteDiskCacheStatusResponse> {
    const id = this.#nextId++;
    const req: GetRemoteDiskCacheStatusRequest = { id, type: "getRemoteDiskCacheStatus" };
    return await new Promise((resolve, reject) => {
      this.#pending.set(id, { resolve, reject });
      this.#worker.postMessage(req);
    });
  }

  async getRemoteDiskTelemetry(): Promise<GetRemoteDiskTelemetryResponse> {
    const id = this.#nextId++;
    const req: GetRemoteDiskTelemetryRequest = { id, type: "getRemoteDiskTelemetry" };
    return await new Promise((resolve, reject) => {
      this.#pending.set(id, { resolve, reject });
      this.#worker.postMessage(req);
    });
  }

  async clearRemoteDiskCache(): Promise<void> {
    const id = this.#nextId++;
    const req: ClearRemoteDiskCacheRequest = { id, type: "clearRemoteDiskCache" };
    await new Promise((resolve, reject) => {
      this.#pending.set(id, { resolve, reject });
      this.#worker.postMessage(req);
    });
  }

  async flushRemoteDiskCache(): Promise<void> {
    const id = this.#nextId++;
    const req: FlushRemoteDiskCacheRequest = { id, type: "flushRemoteDiskCache" };
    await new Promise((resolve, reject) => {
      this.#pending.set(id, { resolve, reject });
      this.#worker.postMessage(req);
    });
  }

  async closeRemoteDisk(): Promise<void> {
    const id = this.#nextId++;
    const req: CloseRemoteDiskRequest = { id, type: "closeRemoteDisk" };
    await new Promise((resolve, reject) => {
      this.#pending.set(id, { resolve, reject });
      this.#worker.postMessage(req);
    });
  }

  async diskReadIntoSharedMemory(opts: DiskReadRequest): Promise<void> {
    // Serialize disk reads issued from this client so we can safely match a single
    // in-flight response without needing a full event demuxer.
    const run = async () => {
      const diskOffset = Number(opts.diskOffset);
      const guestOffset = Number(opts.guestOffset);
      const length = Number(opts.length);

      if (!Number.isFinite(diskOffset) || diskOffset < 0) throw new Error(`Invalid diskOffset=${opts.diskOffset}`);
      if (!Number.isFinite(guestOffset) || guestOffset < 0) throw new Error(`Invalid guestOffset=${opts.guestOffset}`);
      if (!Number.isFinite(length) || length < 0) throw new Error(`Invalid length=${opts.length}`);

      const guestMemory = opts.guestMemory;
      await this.#ensureIpcInitialized(guestMemory);

      const guestBytes = guestMemory.buffer.byteLength;
      if (guestOffset + length > guestBytes) {
        throw new Error(
          `diskReadIntoSharedMemory out-of-bounds: guestOffset=${guestOffset} length=${length} guestBytes=${guestBytes}`,
        );
      }

      const cmdRing = this.#ioCmdRing;
      const evtRing = this.#ioEvtRing;
      if (!cmdRing || !evtRing) throw new Error("I/O IPC rings unavailable");

      const id = (this.#nextIoIpcId++ >>> 0) || (this.#nextIoIpcId++ >>> 0);
      const cmdBytes = encodeCommand({
        kind: "diskRead",
        id,
        diskOffset: BigInt(diskOffset),
        len: length >>> 0,
        guestOffset: BigInt(guestOffset),
      });

      const deadlineMs = (typeof performance !== "undefined" ? performance.now() : Date.now()) + (opts.timeoutMs ?? 5000);

      while (!cmdRing.tryPush(cmdBytes)) {
        const now = typeof performance !== "undefined" ? performance.now() : Date.now();
        if (now >= deadlineMs) throw new Error("diskReadIntoSharedMemory: timed out pushing command");
        await new Promise((resolve) => setTimeout(resolve, 0));
      }

      // Wait for the matching response.
      // eslint-disable-next-line no-constant-condition
      while (true) {
        while (true) {
          const bytes = evtRing.tryPop();
          if (!bytes) break;
          const evt = decodeEvent(bytes);
          if (evt.kind !== "diskReadResp") continue;

          if (evt.id !== id) {
            throw new Error(`diskReadIntoSharedMemory: unexpected diskReadResp id=${evt.id} expected=${id}`);
          }
          if (!evt.ok) {
            throw new Error(`diskReadIntoSharedMemory failed (errorCode=${evt.errorCode ?? "unknown"})`);
          }
          if ((evt.bytes >>> 0) !== (length >>> 0)) {
            throw new Error(`diskReadIntoSharedMemory short read: expected=${length} actual=${evt.bytes}`);
          }
          return;
        }

        const now = typeof performance !== "undefined" ? performance.now() : Date.now();
        if (now >= deadlineMs) throw new Error("diskReadIntoSharedMemory: timed out waiting for response");

        const res = await evtRing.waitForDataAsync(Math.max(0, deadlineMs - now));
        if (res === "timed-out") throw new Error("diskReadIntoSharedMemory: timed out waiting for response");
      }
    };

    const chained = this.#diskIoChain.then(run, run);
    this.#diskIoChain = chained.then(
      () => undefined,
      () => undefined,
    );
    await chained;
  }

  async #ensureIpcInitialized(guestMemory: WebAssembly.Memory): Promise<void> {
    if (this.#ioCmdRing && this.#ioEvtRing) {
      if (this.#guestMemory !== guestMemory) {
        throw new Error("IoWorkerClient: diskReadIntoSharedMemory called with a different guestMemory instance");
      }
      return;
    }
    if (this.#ipcInitPromise) {
      await this.#ipcInitPromise;
      if (!this.#ioCmdRing || !this.#ioEvtRing) throw new Error("IoWorkerClient IPC init failed");
      if (this.#guestMemory !== guestMemory) {
        throw new Error("IoWorkerClient: diskReadIntoSharedMemory called with a different guestMemory instance");
      }
      return;
    }

    if (!(guestMemory.buffer instanceof SharedArrayBuffer)) {
      throw new Error(
        "diskReadIntoSharedMemory requires a shared WebAssembly.Memory (guestMemory.buffer must be a SharedArrayBuffer).",
      );
    }

    const initPromise = (async () => {
      const controlSab = new SharedArrayBuffer(CONTROL_BYTES);
      const status = new Int32Array(controlSab);
      const guestBytes = guestMemory.buffer.byteLength;
      if (!Number.isFinite(guestBytes) || guestBytes <= 0 || guestBytes > 0xffff_ffff) {
        throw new Error(`Invalid guestMemory size: ${guestBytes}`);
      }

      // The worker shared-memory layout normally comes from `allocateSharedMemorySegments()`,
      // but this client can be used standalone (e.g. remote disk panel + Playwright tests).
      // Set up a minimal "all bytes are guest RAM" layout, plus ring headers, so the I/O worker
      // can service diskRead commands without any additional workers.
      Atomics.store(status, StatusIndex.GuestBase, 0);
      Atomics.store(status, StatusIndex.GuestSize, guestBytes | 0);
      Atomics.store(status, StatusIndex.RuntimeReserved, 0);

      for (const role of ["cpu", "gpu", "io", "jit"] as const) {
        const regions = ringRegionsForWorker(role);
        initControlRing(controlSab, regions.command.byteOffset, regions.command.byteLength);
        initControlRing(controlSab, regions.event.byteOffset, regions.event.byteLength);
      }

      // I/O worker doesn't use VGA framebuffer today; allocate a minimal SAB to satisfy the init contract.
      const vgaFramebuffer = new SharedArrayBuffer(4);
      const ioIpcSab = createIpcBuffer([
        { kind: IO_IPC_CMD_QUEUE_KIND, capacityBytes: IO_IPC_RING_CAPACITY_BYTES },
        { kind: IO_IPC_EVT_QUEUE_KIND, capacityBytes: IO_IPC_RING_CAPACITY_BYTES },
      ]).buffer;

      // The full runtime embeds the demo CPU→GPU shared framebuffer inside guest RAM.
      // For the standalone I/O worker client we don't need that, but the init/shared
      // memory contracts still require a buffer + offset. Reuse the guest SAB with
      // offset 0 to satisfy the contract.
      const sharedFramebuffer = guestMemory.buffer as unknown as SharedArrayBuffer;
      const sharedFramebufferOffsetBytes = 0;

      const segments = {
        control: controlSab,
        guestMemory,
        vgaFramebuffer,
        ioIpc: ioIpcSab,
        sharedFramebuffer,
        sharedFramebufferOffsetBytes,
      };
      const views = createSharedMemoryViews(segments);

      const initMsg: WorkerInitMessage = {
        kind: "init",
        role: "io",
        controlSab,
        guestMemory,
        vgaFramebuffer,
        ioIpcSab,
        sharedFramebuffer,
        sharedFramebufferOffsetBytes,
      };
      this.#worker.postMessage(initMsg);

      this.#guestMemory = guestMemory;
      this.#status = views.status;
      this.#ioCmdRing = openRingByKind(ioIpcSab, IO_IPC_CMD_QUEUE_KIND);
      this.#ioEvtRing = openRingByKind(ioIpcSab, IO_IPC_EVT_QUEUE_KIND);

      const deadlineMs = (typeof performance !== "undefined" ? performance.now() : Date.now()) + 2000;
      while (Atomics.load(views.status, StatusIndex.IoReady) !== 1) {
        const now = typeof performance !== "undefined" ? performance.now() : Date.now();
        if (now >= deadlineMs) throw new Error("Timed out waiting for I/O worker init.");
        await new Promise((resolve) => setTimeout(resolve, 1));
      }
    })();

    this.#ipcInitPromise = initPromise;
    try {
      await initPromise;
    } finally {
      // Leave the promise cached so concurrent callers share it; reset only on `close()`.
    }
  }
}
