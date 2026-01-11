/// <reference lib="webworker" />

import type { AeroConfig } from "../config/aero_config";
import { openRingByKind } from "../ipc/ipc";
import { decodeCommand, encodeEvent, type Command, type Event } from "../ipc/protocol";
import { RingBuffer } from "../ipc/ring_buffer";
import { InputEventType } from "../input/event_queue";
import { perf } from "../perf/perf";
import { installWorkerPerfHandlers } from "../perf/worker";
import { PerfWriter } from "../perf/writer.js";
import { PERF_FRAME_HEADER_FRAME_ID_INDEX } from "../perf/shared.js";
import { initWasmForContext, type WasmApi } from "../runtime/wasm_context";
import {
  IO_IPC_CMD_QUEUE_KIND,
  IO_IPC_EVT_QUEUE_KIND,
  StatusIndex,
  createSharedMemoryViews,
  ringRegionsForWorker,
  setReadyFlag,
} from "../runtime/shared_layout";
import {
  type ConfigAckMessage,
  type ConfigUpdateMessage,
  MessageType,
  type ProtocolMessage,
  type WorkerInitMessage,
} from "../runtime/protocol";
import { openSyncAccessHandleInDedicatedWorker } from "../platform/opfs";
import { DEFAULT_OPFS_DISK_IMAGES_DIRECTORY } from "../storage/disk_image_store";
import type { WorkerOpenToken } from "../storage/disk_image_store";
import type { UsbActionMessage, UsbCompletionMessage, UsbHostAction, UsbSelectedMessage } from "../usb/usb_proxy_protocol";

const ctx = self as unknown as DedicatedWorkerGlobalScope;

void installWorkerPerfHandlers();

type InputBatchMessage = { type: "in:input-batch"; buffer: ArrayBuffer };
type InputBatchRecycleMessage = { type: "in:input-batch-recycle"; buffer: ArrayBuffer };

let role: "cpu" | "gpu" | "io" | "jit" = "io";
let status!: Int32Array;
let guestU8!: Uint8Array;

let commandRing!: RingBuffer;
let eventRing: RingBuffer | null = null;

let ioCmdRing: RingBuffer | null = null;
let ioEvtRing: RingBuffer | null = null;
const pendingIoEvents: Uint8Array[] = [];

const DISK_ERROR_NO_ACTIVE_DISK = 1;
const DISK_ERROR_GUEST_OOB = 2;
const DISK_ERROR_DISK_OFFSET_TOO_LARGE = 3;
const DISK_ERROR_IO_FAILURE = 4;

type UsbHidBridge = InstanceType<WasmApi["UsbHidBridge"]>;
let usbHid: UsbHidBridge | null = null;

let currentConfig: AeroConfig | null = null;
let currentConfigVersion = 0;

let started = false;
let pollTimer: number | undefined;

type OpenActiveDiskRequest = { id: number; type: "openActiveDisk"; token: WorkerOpenToken };
type SetMicrophoneRingBufferMessage = {
  type: "setMicrophoneRingBuffer";
  ringBuffer: SharedArrayBuffer | null;
  /** Actual capture sample rate (AudioContext.sampleRate). */
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

let usbAvailable = false;
let usbDemoNextId = 1;

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
  try {
    const data = ev.data as
      | Partial<WorkerInitMessage>
      | Partial<ConfigUpdateMessage>
      | Partial<InputBatchMessage>
      | Partial<OpenActiveDiskRequest>
      | Partial<SetMicrophoneRingBufferMessage>
      | Partial<UsbSelectedMessage>
      | Partial<UsbCompletionMessage>
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

    if ((data as Partial<UsbSelectedMessage>).type === "usb.selected") {
      const msg = data as UsbSelectedMessage;
      usbAvailable = msg.ok;

      // Dev-only smoke test: once a device is selected on the main thread, request the
      // first 18 bytes of the device descriptor to prove the cross-thread broker works.
      if (msg.ok && import.meta.env.DEV) {
        const id = usbDemoNextId++;
        const action: UsbHostAction = {
          kind: "controlIn",
          id,
          setup: {
            bmRequestType: 0x80, // device-to-host | standard | device
            bRequest: 0x06, // GET_DESCRIPTOR
            wValue: 0x0100, // DEVICE descriptor (1) index 0
            wIndex: 0x0000,
            wLength: 18,
          },
        };
        ctx.postMessage({ type: "usb.action", action } satisfies UsbActionMessage);
      }
      return;
    }

    if ((data as Partial<UsbCompletionMessage>).type === "usb.completion") {
      const msg = data as UsbCompletionMessage;
      if (import.meta.env.DEV) {
        if (msg.completion.kind === "okIn") {
          console.log("[io.worker] WebUSB completion okIn", msg.completion.id, Array.from(msg.completion.data));
        } else {
          console.log("[io.worker] WebUSB completion", msg.completion);
        }
      }
      return;
    }

    // First message is the shared-memory init handshake.
    if ((data as Partial<WorkerInitMessage>).kind === "init") {
      const init = data as WorkerInitMessage;
      perf.spanBegin("worker:boot");
      try {
        void perf.spanAsync("wasm:init", async () => {
          try {
            const { api } = await initWasmForContext({
              variant: init.wasmVariant ?? "auto",
              module: init.wasmModule,
              memory: init.guestMemory,
            });
            usbHid = new api.UsbHidBridge();
          } catch (err) {
            const message = err instanceof Error ? err.message : String(err);
            console.error(`[io.worker] wasm:init failed: ${message}`);
            pushEvent({ kind: "log", level: "error", message: `wasm:init failed: ${message}` });
          }
        });

        perf.spanBegin("worker:init");
        try {
          role = init.role ?? "io";
          const segments = {
            control: init.controlSab!,
            guestMemory: init.guestMemory!,
            vgaFramebuffer: init.vgaFramebuffer!,
            ioIpc: init.ioIpcSab!,
          };
          const views = createSharedMemoryViews(segments);
          status = views.status;
          guestU8 = views.guestU8;
          const regions = ringRegionsForWorker(role);
          commandRing = new RingBuffer(segments.control, regions.command.byteOffset);
          eventRing = new RingBuffer(segments.control, regions.event.byteOffset);
          ioCmdRing = openRingByKind(segments.ioIpc, IO_IPC_CMD_QUEUE_KIND);
          ioEvtRing = openRingByKind(segments.ioIpc, IO_IPC_EVT_QUEUE_KIND);

          if (init.perfChannel) {
            perfWriter = new PerfWriter(init.perfChannel.buffer, {
              workerKind: init.perfChannel.workerKind,
              runStartEpochMs: init.perfChannel.runStartEpochMs,
            });
            perfFrameHeader = new Int32Array(init.perfChannel.frameHeader);
            perfLastFrameId = 0;
            perfIoMs = 0;
            perfIoReadBytes = 0;
            perfIoWriteBytes = 0;
          }
          pushEvent({ kind: "log", level: "info", message: "worker ready" });

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
  } catch (err) {
    fatal(err);
  }
};

function startPolling(): void {
  if (started) return;
  started = true;

  // This worker must remain responsive to `postMessage` input batches. Avoid blocking loops / Atomics.wait
  // here; instead poll the command ring at a low rate.
  pollTimer = setInterval(() => {
    const t0 = performance.now();
    drainRuntimeCommands();
    drainIoIpcCommands();
    perfIoMs += performance.now() - t0;
    maybeEmitPerfSample();
    if (Atomics.load(status, StatusIndex.StopRequested) === 1) {
      shutdown();
    }
  }, 8) as unknown as number;
}

function drainRuntimeCommands(): void {
  while (true) {
    const bytes = commandRing.tryPop();
    if (!bytes) break;
    let cmd: Command;
    try {
      cmd = decodeCommand(bytes);
    } catch {
      continue;
    }
    if (cmd.kind === "shutdown") {
      Atomics.store(status, StatusIndex.StopRequested, 1);
    }
  }
}

function drainIoIpcCommands(): void {
  const cmdRing = ioCmdRing;
  if (!cmdRing) return;
  flushPendingIoEvents();

  while (true) {
    const bytes = cmdRing.tryPop();
    if (!bytes) break;

    let cmd: Command;
    try {
      cmd = decodeCommand(bytes);
    } catch {
      continue;
    }

    if (cmd.kind === "diskRead") {
      handleDiskRead(cmd);
    } else if (cmd.kind === "diskWrite") {
      handleDiskWrite(cmd);
    }
  }
}

function flushPendingIoEvents(): void {
  const evtRing = ioEvtRing;
  if (!evtRing) return;
  while (pendingIoEvents.length > 0) {
    const bytes = pendingIoEvents[0]!;
    if (!evtRing.tryPush(bytes)) break;
    pendingIoEvents.shift();
  }
}

function enqueueIoEvent(bytes: Uint8Array): void {
  const evtRing = ioEvtRing;
  if (!evtRing) return;
  if (evtRing.tryPush(bytes)) return;
  pendingIoEvents.push(bytes);
}

function diskOffsetToJsNumber(diskOffset: bigint, len: number): number | null {
  if (diskOffset < 0n) return null;
  const end = diskOffset + BigInt(len >>> 0);
  if (diskOffset > BigInt(Number.MAX_SAFE_INTEGER) || end > BigInt(Number.MAX_SAFE_INTEGER)) return null;
  return Number(diskOffset);
}

function guestRangeView(guestOffset: bigint, len: number): Uint8Array | null {
  const guestBytes = BigInt(guestU8.byteLength);
  if (guestOffset < 0n) return null;
  const end = guestOffset + BigInt(len >>> 0);
  if (end > guestBytes) return null;
  const start = Number(guestOffset);
  return guestU8.subarray(start, start + (len >>> 0));
}

function handleDiskRead(cmd: Extract<Command, { kind: "diskRead" }>): void {
  const id = cmd.id >>> 0;
  const len = cmd.len >>> 0;
  const handle = activeAccessHandle;
  if (!handle) {
    enqueueIoEvent(encodeEvent({ kind: "diskReadResp", id, ok: false, bytes: 0, errorCode: DISK_ERROR_NO_ACTIVE_DISK }));
    return;
  }

  const view = guestRangeView(cmd.guestOffset, len);
  if (!view) {
    enqueueIoEvent(encodeEvent({ kind: "diskReadResp", id, ok: false, bytes: 0, errorCode: DISK_ERROR_GUEST_OOB }));
    return;
  }

  const at = diskOffsetToJsNumber(cmd.diskOffset, len);
  if (at === null) {
    enqueueIoEvent(
      encodeEvent({ kind: "diskReadResp", id, ok: false, bytes: 0, errorCode: DISK_ERROR_DISK_OFFSET_TOO_LARGE }),
    );
    return;
  }

  try {
    const bytes = handle.read(view, { at });
    perfIoReadBytes += bytes >>> 0;
    enqueueIoEvent(encodeEvent({ kind: "diskReadResp", id, ok: true, bytes: bytes >>> 0 }));
  } catch {
    enqueueIoEvent(encodeEvent({ kind: "diskReadResp", id, ok: false, bytes: 0, errorCode: DISK_ERROR_IO_FAILURE }));
  }
}

function handleDiskWrite(cmd: Extract<Command, { kind: "diskWrite" }>): void {
  const id = cmd.id >>> 0;
  const len = cmd.len >>> 0;
  const handle = activeAccessHandle;
  if (!handle) {
    enqueueIoEvent(
      encodeEvent({ kind: "diskWriteResp", id, ok: false, bytes: 0, errorCode: DISK_ERROR_NO_ACTIVE_DISK }),
    );
    return;
  }

  const view = guestRangeView(cmd.guestOffset, len);
  if (!view) {
    enqueueIoEvent(encodeEvent({ kind: "diskWriteResp", id, ok: false, bytes: 0, errorCode: DISK_ERROR_GUEST_OOB }));
    return;
  }

  const at = diskOffsetToJsNumber(cmd.diskOffset, len);
  if (at === null) {
    enqueueIoEvent(
      encodeEvent({ kind: "diskWriteResp", id, ok: false, bytes: 0, errorCode: DISK_ERROR_DISK_OFFSET_TOO_LARGE }),
    );
    return;
  }

  try {
    const bytes = handle.write(view, { at });
    perfIoWriteBytes += bytes >>> 0;
    enqueueIoEvent(encodeEvent({ kind: "diskWriteResp", id, ok: true, bytes: bytes >>> 0 }));
  } catch {
    enqueueIoEvent(encodeEvent({ kind: "diskWriteResp", id, ok: false, bytes: 0, errorCode: DISK_ERROR_IO_FAILURE }));
  }
}

function handleInputBatch(buffer: ArrayBuffer): void {
  const t0 = performance.now();
  // `buffer` is transferred from the main thread, so it is uniquely owned here.
  const words = new Int32Array(buffer);
  const count = words[0] >>> 0;

  Atomics.add(status, StatusIndex.IoInputBatchCounter, 1);
  Atomics.add(status, StatusIndex.IoInputEventCounter, count);

  // The actual i8042 device model is implemented in Rust; this worker currently
  // only wires the browser's input batches into the USB HID models (for the UHCI
  // path) while retaining PS/2 scancode events for the legacy path.
  const base = 2;
  for (let i = 0; i < count; i++) {
    const off = base + i * 4;
    const type = words[off] >>> 0;
    switch (type) {
      case InputEventType.KeyHidUsage: {
        const packed = words[off + 2] >>> 0;
        const usage = packed & 0xff;
        const pressed = ((packed >>> 8) & 1) !== 0;
        usbHid?.keyboard_event(usage, pressed);
        break;
      }
      case InputEventType.MouseMove: {
        const dx = words[off + 2] | 0;
        const dyPs2 = words[off + 3] | 0;
        // PS/2 convention: positive is up. HID convention: positive is down.
        usbHid?.mouse_move(dx, -dyPs2);
        break;
      }
      case InputEventType.MouseButtons: {
        usbHid?.mouse_buttons(words[off + 2] & 0xff);
        break;
      }
      case InputEventType.MouseWheel: {
        usbHid?.mouse_wheel(words[off + 2] | 0);
        break;
      }
      case InputEventType.GamepadReport:
        // HID gamepad report: a/b are packed 8 bytes (little-endian).
        usbHid?.gamepad_report(words[off + 2] >>> 0, words[off + 3] >>> 0);
        break;
      case InputEventType.KeyScancode:
        // Key payload is packed bytes + len. No-op for now.
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
  usbHid?.free();
  usbHid = null;
  pushEvent({ kind: "log", level: "info", message: "worker shutdown" });
  setReadyFlag(status, role, false);
  ctx.close();
}

void currentConfig;

function pushEvent(evt: Event): void {
  if (!eventRing) return;
  eventRing.tryPush(encodeEvent(evt));
}

function pushEventBlocking(evt: Event, timeoutMs = 1000): void {
  if (!eventRing) return;
  const payload = encodeEvent(evt);
  if (eventRing.tryPush(payload)) return;
  try {
    eventRing.pushBlocking(payload, timeoutMs);
  } catch {
    // ignore
  }
}

function fatal(err: unknown): void {
  const message = err instanceof Error ? err.message : String(err);
  pushEventBlocking({ kind: "panic", message });
  try {
    setReadyFlag(status, role, false);
  } catch {
    // ignore if we haven't initialized shared memory yet.
  }
  ctx.postMessage({ type: MessageType.ERROR, role, message } satisfies ProtocolMessage);
  ctx.close();
}
