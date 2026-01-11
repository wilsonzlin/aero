/// <reference lib="webworker" />

import type { AeroConfig } from "../config/aero_config";
import { InputEventType } from "../input/event_queue";
import { perf } from "../perf/perf";
import { installWorkerPerfHandlers } from "../perf/worker";
import { PerfWriter } from "../perf/writer.js";
import { PERF_FRAME_HEADER_FRAME_ID_INDEX } from "../perf/shared.js";
import { RingBuffer } from "../runtime/ring_buffer";
import { StatusIndex, createSharedMemoryViews, ringRegionsForWorker, setReadyFlag } from "../runtime/shared_layout";
import {
  type ConfigAckMessage,
  type ConfigUpdateMessage,
  MessageType,
  type ProtocolMessage,
  type WorkerInitMessage,
  decodeProtocolMessage,
} from "../runtime/protocol";
import { openSyncAccessHandleInDedicatedWorker } from "../platform/opfs";
import { DEFAULT_OPFS_DISK_IMAGES_DIRECTORY } from "../storage/disk_image_store";
import type { WorkerOpenToken } from "../storage/disk_image_store";

const ctx = self as unknown as DedicatedWorkerGlobalScope;

void installWorkerPerfHandlers();

type InputBatchMessage = { type: "in:input-batch"; buffer: ArrayBuffer };
type InputBatchRecycleMessage = { type: "in:input-batch-recycle"; buffer: ArrayBuffer };

let role: "cpu" | "gpu" | "io" | "jit" = "io";
let status!: Int32Array;
let commandRing!: RingBuffer;

let currentConfig: AeroConfig | null = null;
let currentConfigVersion = 0;

let started = false;
let pollTimer: number | undefined;

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

let perfWriter: PerfWriter | null = null;
let perfFrameHeader: Int32Array | null = null;
let perfLastFrameId = 0;
let perfIoMs = 0;
let perfIoReadBytes = 0;
let perfIoWriteBytes = 0;

function maybeEmitPerfSample(): void {
  if (!perfWriter || !perfFrameHeader) return;
  const frameId = Atomics.load(perfFrameHeader, PERF_FRAME_HEADER_FRAME_ID_INDEX) >>> 0;
  if (frameId === 0 || frameId === perfLastFrameId) return;
  perfLastFrameId = frameId;

  const ioMs = perfIoMs > 0 ? perfIoMs : 0.01;
  perfWriter.frameSample(frameId, {
    durations: { io_ms: ioMs },
    counters: {
      io_read_bytes: perfIoReadBytes,
      io_write_bytes: perfIoWriteBytes,
    },
  });

  perfIoMs = 0;
  perfIoReadBytes = 0;
  perfIoWriteBytes = 0;
}

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
  const t0 = performance.now();
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
  } finally {
    perfIoMs += performance.now() - t0;
  }
}

ctx.onmessage = (ev: MessageEvent<unknown>) => {
  const data = ev.data as
    | Partial<WorkerInitMessage>
    | Partial<ConfigUpdateMessage>
    | Partial<InputBatchMessage>
    | Partial<OpenActiveDiskRequest>
    | Partial<SetMicrophoneRingBufferMessage>
    | undefined;
  if (!data) return;

  if ((data as Partial<ConfigUpdateMessage>).kind === "config.update") {
    const update = data as ConfigUpdateMessage;
    currentConfig = update.config;
    currentConfigVersion = update.version;
    ctx.postMessage({ kind: "config.ack", version: currentConfigVersion } satisfies ConfigAckMessage);
    return;
  }

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
    const init = data as WorkerInitMessage;
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

        if (init.perfChannel) {
          perfWriter = new PerfWriter(init.perfChannel.buffer, {
            workerKind: init.perfChannel.workerKind,
            runStartEpochMs: init.perfChannel.runStartEpochMs,
          });
          perfFrameHeader = new Int32Array(init.perfChannel.frameHeader);
        }

        setReadyFlag(status, role, true);
        ctx.postMessage({ type: MessageType.READY, role } satisfies ProtocolMessage);
        if (perf.traceEnabled) perf.instant("boot:worker:ready", "p", { role });
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
    const buffer = msg.buffer;
    if (started) {
      handleInputBatch(buffer);
    }
    if ((msg as { recycle?: unknown }).recycle === true) {
      ctx.postMessage({ type: "in:input-batch-recycle", buffer } satisfies InputBatchRecycleMessage, [buffer]);
    }
    return;
  }
};

function startPolling(): void {
  if (started) return;
  started = true;

  // This worker must remain responsive to `postMessage` input batches. Avoid blocking loops / Atomics.wait
  // here; instead poll the command ring at a low rate.
  pollTimer = setInterval(() => {
    const t0 = performance.now();
    drainCommands();
    perfIoMs += performance.now() - t0;
    maybeEmitPerfSample();
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
  const t0 = performance.now();
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
    switch (type) {
      case InputEventType.KeyScancode:
        // Key payload is packed bytes + len. No-op for now.
        break;
      case InputEventType.MouseMove:
      case InputEventType.MouseButtons:
      case InputEventType.MouseWheel:
        // PS/2 mouse events (not yet wired into the device model here).
        break;
      case InputEventType.GamepadReport:
        // HID gamepad report: a/b are packed 8 bytes.
        break;
      default:
        // Unknown event type; ignore.
        break;
    }
  }

  perfIoReadBytes += buffer.byteLength;
  perfIoMs += performance.now() - t0;
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

void currentConfig;
