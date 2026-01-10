/// <reference lib="webworker" />

import { perf } from "../perf/perf";
import { installWorkerPerfHandlers } from "../perf/worker";
import { RingBuffer } from "../runtime/ring_buffer";
import { StatusIndex, createSharedMemoryViews, ringRegionsForWorker, setReadyFlag } from "../runtime/shared_layout";
import { MessageType, type ProtocolMessage, type WorkerInitMessage, decodeProtocolMessage } from "../runtime/protocol";
import { InputEventType } from "../input/event_queue";
import { openSyncAccessHandleInDedicatedWorker } from "../platform/opfs";
import { DEFAULT_OPFS_DISK_IMAGES_DIRECTORY } from "../storage/disk_image_store";
import type { WorkerOpenToken } from "../storage/disk_image_store";

const ctx = self as unknown as DedicatedWorkerGlobalScope;

void installWorkerPerfHandlers();

let role: "cpu" | "gpu" | "io" | "jit" = "io";
let status!: Int32Array;
let commandRing!: RingBuffer;

let started = false;
let pollTimer: number | undefined;

type InputBatchMessage = { type: "in:input-batch"; buffer: ArrayBuffer };
type OpenActiveDiskRequest = { id: number; type: "openActiveDisk"; token: WorkerOpenToken };
type SetMicrophoneRingBufferMessage = {
  type: "setMicrophoneRingBuffer";
  ringBuffer: SharedArrayBuffer | null;
  sampleRate?: number;
};

type OpenActiveDiskResult =
  | {
      id: number;
      type: "openActiveDiskResult";
      ok: true;
      size: number;
      syncAccessHandleAvailable: boolean;
    }
  | {
      id: number;
      type: "openActiveDiskResult";
      ok: false;
      error: string;
    };

let activeAccessHandle: FileSystemSyncAccessHandle | null = null;

let micRingBuffer: SharedArrayBuffer | null = null;
let micSampleRate = 0;

function attachMicRingBuffer(ringBuffer: SharedArrayBuffer | null, sampleRate?: number): void {
  if (ringBuffer !== null) {
    const Sab = globalThis.SharedArrayBuffer;
    if (typeof Sab === "undefined") {
      throw new Error("SharedArrayBuffer is unavailable; microphone capture requires crossOriginIsolated.");
    }
    if (!(ringBuffer instanceof Sab)) {
      throw new Error("setMicrophoneRingBuffer expects a SharedArrayBuffer or null.");
    }
  }

  micRingBuffer = ringBuffer;
  micSampleRate = (sampleRate ?? 0) | 0;
}

async function openOpfsDisk(directory: string, name: string): Promise<{ size: number }> {
  const dirToUse = directory || DEFAULT_OPFS_DISK_IMAGES_DIRECTORY;
  const path = `${dirToUse}/${name}`;
  activeAccessHandle?.close();
  activeAccessHandle = await openSyncAccessHandleInDedicatedWorker(path, { create: false });
  return { size: activeAccessHandle.getSize() };
}

async function handleOpenActiveDisk(msg: OpenActiveDiskRequest): Promise<void> {
  try {
    if (msg.token.kind === "opfs") {
      const { size } = await openOpfsDisk(msg.token.directory, msg.token.name);
      const res: OpenActiveDiskResult = {
        id: msg.id,
        type: "openActiveDiskResult",
        ok: true,
        size,
        syncAccessHandleAvailable: true,
      };
      ctx.postMessage(res);
      return;
    }

    const res: OpenActiveDiskResult = {
      id: msg.id,
      type: "openActiveDiskResult",
      ok: true,
      size: msg.token.blob.size,
      syncAccessHandleAvailable: false,
    };
    ctx.postMessage(res);
  } catch (err) {
    const res: OpenActiveDiskResult = {
      id: msg.id,
      type: "openActiveDiskResult",
      ok: false,
      error: err instanceof Error ? err.message : String(err),
    };
    ctx.postMessage(res);
  }
}

ctx.onmessage = (ev: MessageEvent<unknown>) => {
  const data = ev.data as
    | Partial<WorkerInitMessage>
    | Partial<InputBatchMessage>
    | Partial<OpenActiveDiskRequest>
    | Partial<SetMicrophoneRingBufferMessage>
    | undefined;
  if (!data) return;

  if ((data as Partial<OpenActiveDiskRequest>).type === "openActiveDisk") {
    void handleOpenActiveDisk(data as OpenActiveDiskRequest);
    return;
  }

  if ((data as Partial<SetMicrophoneRingBufferMessage>).type === "setMicrophoneRingBuffer") {
    const msg = data as Partial<SetMicrophoneRingBufferMessage>;
    attachMicRingBuffer((msg.ringBuffer as SharedArrayBuffer | null) ?? null, msg.sampleRate);
    return;
  }

  // First message is the shared-memory init handshake.
  if ((data as Partial<WorkerInitMessage>).kind === "init") {
    const init = data as Partial<WorkerInitMessage>;
    perf.spanBegin("worker:boot");
    try {
      perf.spanBegin("wasm:init");
      perf.spanEnd("wasm:init");

      perf.spanBegin("worker:init");
      try {
        role = init.role ?? "io";
        const segments = { control: init.controlSab!, guestMemory: init.guestMemory!, vgaFramebuffer: init.vgaFramebuffer! };
        status = createSharedMemoryViews(segments).status;
        const regions = ringRegionsForWorker(role);
        commandRing = new RingBuffer(segments.control, regions.command.byteOffset, regions.command.byteLength);

        setReadyFlag(status, role, true);
        ctx.postMessage({ type: MessageType.READY, role } satisfies ProtocolMessage);
        perf.instant("boot:worker:ready", "p", { role });
      } finally {
        perf.spanEnd("worker:init");
      }
    } finally {
      perf.spanEnd("worker:boot");
    }

    startPolling();
    return;
  }

  // Input is delivered via structured `postMessage` to avoid SharedArrayBuffer contention on the
  // main thread and to keep the hot path in JS simple.
  if ((data as Partial<InputBatchMessage>).type === "in:input-batch") {
    const msg = data as Partial<InputBatchMessage>;
    if (!(msg.buffer instanceof ArrayBuffer)) return;
    if (!started) return;
    handleInputBatch(msg.buffer);
    return;
  }
};

function startPolling(): void {
  if (started) return;
  started = true;

  // This worker must remain responsive to `postMessage` input batches. Avoid blocking loops / Atomics.wait
  // here; instead poll the command ring at a low rate.
  pollTimer = setInterval(() => {
    drainCommands();
    if (Atomics.load(status, StatusIndex.StopRequested) === 1) {
      shutdown();
    }
  }, 8) as unknown as number;
}

function drainCommands(): void {
  while (true) {
    const bytes = commandRing.pop();
    if (!bytes) break;
    const cmd = decodeProtocolMessage(bytes);
    if (!cmd) continue;
    if (cmd.type === MessageType.STOP) {
      Atomics.store(status, StatusIndex.StopRequested, 1);
    }
  }
}

function handleInputBatch(buffer: ArrayBuffer): void {
  // `buffer` is transferred from the main thread, so it is uniquely owned here.
  const words = new Int32Array(buffer);
  const count = words[0] >>> 0;

  Atomics.add(status, StatusIndex.IoInputBatchCounter, 1);
  Atomics.add(status, StatusIndex.IoInputEventCounter, count);

  // The actual i8042 device model is implemented in Rust; once the WASM-side I/O devices are
  // integrated, this loop should feed those devices directly.
  // For now, we only track counters so tests can assert that the worker is receiving input.
  const base = 2;
  for (let i = 0; i < count; i++) {
    const off = base + i * 4;
    const type = words[off] >>> 0;
    if (type === InputEventType.KeyScancode) {
      // Key payload is packed bytes + len. No-op for now.
      continue;
    }
  }
}

function shutdown(): void {
  if (pollTimer !== undefined) {
    clearInterval(pollTimer);
    pollTimer = undefined;
  }

  activeAccessHandle?.close();
  setReadyFlag(status, role, false);
  ctx.close();
}
