/// <reference lib="webworker" />

import type { AeroConfig } from "../config/aero_config";
import { openRingByKind } from "../ipc/ipc";
import { decodeCommand, encodeEvent, type Command, type Event } from "../ipc/protocol";
import { RingBuffer } from "../ipc/ring_buffer";
import { InputEventType } from "../input/event_queue";
import { perf } from "../perf/perf";
import { installWorkerPerfHandlers } from "../perf/worker";
import { PerfWriter } from "../perf/writer.js";
import { PERF_FRAME_HEADER_ENABLED_INDEX, PERF_FRAME_HEADER_FRAME_ID_INDEX } from "../perf/shared.js";
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
import { DeviceManager, type IrqSink } from "../io/device_manager";
import { I8042Controller } from "../io/devices/i8042";
import { PciTestDevice } from "../io/devices/pci_test_device";
import { UART_COM1, Uart16550, type SerialOutputSink } from "../io/devices/uart16550";
import { openSyncAccessHandleInDedicatedWorker } from "../platform/opfs.ts";
import { RemoteStreamingDisk, type RemoteDiskCacheStatus, type RemoteDiskTelemetrySnapshot } from "../platform/remote_disk";
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
const DISK_ERROR_READ_ONLY = 5;
const DISK_ERROR_DISK_OOB = 6;

let deviceManager: DeviceManager | null = null;
let i8042: I8042Controller | null = null;

let portReadCount = 0;
let portWriteCount = 0;
let mmioReadCount = 0;
let mmioWriteCount = 0;

type UsbHidBridge = InstanceType<WasmApi["UsbHidBridge"]>;
let usbHid: UsbHidBridge | null = null;

let currentConfig: AeroConfig | null = null;
let currentConfigVersion = 0;

let started = false;
let pollTimer: number | undefined;

type OpenActiveDiskRequest = { id: number; type: "openActiveDisk"; token: WorkerOpenToken };
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
type GetRemoteDiskCacheStatusRequest = { id: number; type: "getRemoteDiskCacheStatus" };
type GetRemoteDiskTelemetryRequest = { id: number; type: "getRemoteDiskTelemetry" };
type ClearRemoteDiskCacheRequest = { id: number; type: "clearRemoteDiskCache" };
type FlushRemoteDiskCacheRequest = { id: number; type: "flushRemoteDiskCache" };
type CloseRemoteDiskRequest = { id: number; type: "closeRemoteDisk" };
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

type OpenRemoteDiskResult =
  | { id: number; type: "openRemoteDiskResult"; ok: true; size: number }
  | { id: number; type: "openRemoteDiskResult"; ok: false; error: string };

type GetRemoteDiskCacheStatusResult =
  | { id: number; type: "getRemoteDiskCacheStatusResult"; ok: true; status: RemoteDiskCacheStatus }
  | { id: number; type: "getRemoteDiskCacheStatusResult"; ok: false; error: string };

type GetRemoteDiskTelemetryResult =
  | { id: number; type: "getRemoteDiskTelemetryResult"; ok: true; telemetry: RemoteDiskTelemetrySnapshot }
  | { id: number; type: "getRemoteDiskTelemetryResult"; ok: false; error: string };

type ClearRemoteDiskCacheResult =
  | { id: number; type: "clearRemoteDiskCacheResult"; ok: true }
  | { id: number; type: "clearRemoteDiskCacheResult"; ok: false; error: string };

type FlushRemoteDiskCacheResult =
  | { id: number; type: "flushRemoteDiskCacheResult"; ok: true }
  | { id: number; type: "flushRemoteDiskCacheResult"; ok: false; error: string };

type CloseRemoteDiskResult =
  | { id: number; type: "closeRemoteDiskResult"; ok: true }
  | { id: number; type: "closeRemoteDiskResult"; ok: false; error: string };

let activeAccessHandle: FileSystemSyncAccessHandle | null = null;
let activeDiskCapacityBytes: number | null = null;
let activeRemoteDisk: RemoteStreamingDisk | null = null;
let remoteDiskReadChain: Promise<void> = Promise.resolve();

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
  const enabled = Atomics.load(perfFrameHeader, PERF_FRAME_HEADER_ENABLED_INDEX) !== 0;
  const frameId = Atomics.load(perfFrameHeader, PERF_FRAME_HEADER_FRAME_ID_INDEX) >>> 0;
  if (!enabled) {
    perfLastFrameId = frameId;
    perfIoMs = 0;
    perfIoReadBytes = 0;
    perfIoWriteBytes = 0;
    return;
  }
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
  await closeActiveRemoteDisk();
  activeAccessHandle?.close();
  activeDiskCapacityBytes = null;
  activeAccessHandle = await openSyncAccessHandleInDedicatedWorker(path, { create: false });
  activeDiskCapacityBytes = activeAccessHandle.getSize();
  return { size: activeDiskCapacityBytes };
}

async function closeActiveRemoteDisk(): Promise<void> {
  const disk = activeRemoteDisk;
  if (!disk) return;
  activeRemoteDisk = null;
  activeDiskCapacityBytes = null;
  remoteDiskReadChain = Promise.resolve();
  try {
    await disk.close();
  } catch {
    // Best-effort cleanup; ignore errors.
  }
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

async function handleOpenRemoteDisk(msg: OpenRemoteDiskRequest): Promise<void> {
  const t0 = performance.now();
  try {
    const url = msg.url.trim();
    if (!url) throw new Error("openRemoteDisk: url is required");

    const blockSize = msg.options?.blockSize;
    const cacheLimitMiB = msg.options?.cacheLimitMiB;
    const cacheLimitBytes =
      cacheLimitMiB === null || (typeof cacheLimitMiB === "number" && cacheLimitMiB <= 0)
        ? null
        : typeof cacheLimitMiB === "number"
          ? cacheLimitMiB * 1024 * 1024
          : undefined;

    await closeActiveRemoteDisk();
    activeAccessHandle?.close();
    activeAccessHandle = null;
    activeDiskCapacityBytes = null;

    const disk = await RemoteStreamingDisk.open(url, {
      blockSize,
      cacheLimitBytes,
      credentials: msg.options?.credentials,
      prefetchSequentialBlocks: msg.options?.prefetchSequentialBlocks,
      cacheBackend: msg.options?.cacheBackend,
      cacheImageId: msg.options?.cacheImageId,
      cacheVersion: msg.options?.cacheVersion,
    });
    activeRemoteDisk = disk;
    activeDiskCapacityBytes = disk.capacityBytes;
    remoteDiskReadChain = Promise.resolve();

    const res: OpenRemoteDiskResult = { id: msg.id, type: "openRemoteDiskResult", ok: true, size: disk.capacityBytes };
    ctx.postMessage(res);
  } catch (err) {
    const res: OpenRemoteDiskResult = {
      id: msg.id,
      type: "openRemoteDiskResult",
      ok: false,
      error: err instanceof Error ? err.message : String(err),
    };
    ctx.postMessage(res);
  } finally {
    perfIoMs += performance.now() - t0;
  }
}

async function handleGetRemoteDiskCacheStatus(msg: GetRemoteDiskCacheStatusRequest): Promise<void> {
  const t0 = performance.now();
  try {
    if (!activeRemoteDisk) throw new Error("No remote disk is open.");
    const status = await activeRemoteDisk.getCacheStatus();
    const res: GetRemoteDiskCacheStatusResult = { id: msg.id, type: "getRemoteDiskCacheStatusResult", ok: true, status };
    ctx.postMessage(res);
  } catch (err) {
    const res: GetRemoteDiskCacheStatusResult = {
      id: msg.id,
      type: "getRemoteDiskCacheStatusResult",
      ok: false,
      error: err instanceof Error ? err.message : String(err),
    };
    ctx.postMessage(res);
  } finally {
    perfIoMs += performance.now() - t0;
  }
}

async function handleGetRemoteDiskTelemetry(msg: GetRemoteDiskTelemetryRequest): Promise<void> {
  const t0 = performance.now();
  try {
    if (!activeRemoteDisk) throw new Error("No remote disk is open.");
    const telemetry = activeRemoteDisk.getTelemetrySnapshot();
    const res: GetRemoteDiskTelemetryResult = {
      id: msg.id,
      type: "getRemoteDiskTelemetryResult",
      ok: true,
      telemetry,
    };
    ctx.postMessage(res);
  } catch (err) {
    const res: GetRemoteDiskTelemetryResult = {
      id: msg.id,
      type: "getRemoteDiskTelemetryResult",
      ok: false,
      error: err instanceof Error ? err.message : String(err),
    };
    ctx.postMessage(res);
  } finally {
    perfIoMs += performance.now() - t0;
  }
}

async function handleClearRemoteDiskCache(msg: ClearRemoteDiskCacheRequest): Promise<void> {
  const t0 = performance.now();
  try {
    if (!activeRemoteDisk) throw new Error("No remote disk is open.");
    await activeRemoteDisk.clearCache();
    const res: ClearRemoteDiskCacheResult = { id: msg.id, type: "clearRemoteDiskCacheResult", ok: true };
    ctx.postMessage(res);
  } catch (err) {
    const res: ClearRemoteDiskCacheResult = {
      id: msg.id,
      type: "clearRemoteDiskCacheResult",
      ok: false,
      error: err instanceof Error ? err.message : String(err),
    };
    ctx.postMessage(res);
  } finally {
    perfIoMs += performance.now() - t0;
  }
}

async function handleFlushRemoteDiskCache(msg: FlushRemoteDiskCacheRequest): Promise<void> {
  const t0 = performance.now();
  try {
    if (!activeRemoteDisk) throw new Error("No remote disk is open.");
    await activeRemoteDisk.flushCache();
    const res: FlushRemoteDiskCacheResult = { id: msg.id, type: "flushRemoteDiskCacheResult", ok: true };
    ctx.postMessage(res);
  } catch (err) {
    const res: FlushRemoteDiskCacheResult = {
      id: msg.id,
      type: "flushRemoteDiskCacheResult",
      ok: false,
      error: err instanceof Error ? err.message : String(err),
    };
    ctx.postMessage(res);
  } finally {
    perfIoMs += performance.now() - t0;
  }
}

async function handleCloseRemoteDisk(msg: CloseRemoteDiskRequest): Promise<void> {
  const t0 = performance.now();
  try {
    if (!activeRemoteDisk) throw new Error("No remote disk is open.");
    await closeActiveRemoteDisk();
    const res: CloseRemoteDiskResult = { id: msg.id, type: "closeRemoteDiskResult", ok: true };
    ctx.postMessage(res);
  } catch (err) {
    const res: CloseRemoteDiskResult = {
      id: msg.id,
      type: "closeRemoteDiskResult",
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
      | Partial<OpenRemoteDiskRequest>
      | Partial<GetRemoteDiskCacheStatusRequest>
      | Partial<GetRemoteDiskTelemetryRequest>
      | Partial<ClearRemoteDiskCacheRequest>
      | Partial<FlushRemoteDiskCacheRequest>
      | Partial<CloseRemoteDiskRequest>
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

    if ((data as Partial<OpenRemoteDiskRequest>).type === "openRemoteDisk") {
      void handleOpenRemoteDisk(data as OpenRemoteDiskRequest);
      return;
    }

    if ((data as Partial<GetRemoteDiskCacheStatusRequest>).type === "getRemoteDiskCacheStatus") {
      void handleGetRemoteDiskCacheStatus(data as GetRemoteDiskCacheStatusRequest);
      return;
    }

    if ((data as Partial<GetRemoteDiskTelemetryRequest>).type === "getRemoteDiskTelemetry") {
      void handleGetRemoteDiskTelemetry(data as GetRemoteDiskTelemetryRequest);
      return;
    }

    if ((data as Partial<ClearRemoteDiskCacheRequest>).type === "clearRemoteDiskCache") {
      void handleClearRemoteDiskCache(data as ClearRemoteDiskCacheRequest);
      return;
    }

    if ((data as Partial<FlushRemoteDiskCacheRequest>).type === "flushRemoteDiskCache") {
      void handleFlushRemoteDiskCache(data as FlushRemoteDiskCacheRequest);
      return;
    }

    if ((data as Partial<CloseRemoteDiskRequest>).type === "closeRemoteDisk") {
      void handleCloseRemoteDisk(data as CloseRemoteDiskRequest);
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
            sharedFramebuffer: init.sharedFramebuffer!,
            sharedFramebufferOffsetBytes: init.sharedFramebufferOffsetBytes ?? 0,
          };
          const views = createSharedMemoryViews(segments);
          status = views.status;
          guestU8 = views.guestU8;
          const regions = ringRegionsForWorker(role);
          commandRing = new RingBuffer(segments.control, regions.command.byteOffset);
          eventRing = new RingBuffer(segments.control, regions.event.byteOffset);
          ioCmdRing = openRingByKind(segments.ioIpc, IO_IPC_CMD_QUEUE_KIND);
          ioEvtRing = openRingByKind(segments.ioIpc, IO_IPC_EVT_QUEUE_KIND);

          const irqSink: IrqSink = {
            raiseIrq: (irq) => enqueueIoEvent(encodeEvent({ kind: "irqRaise", irq: irq & 0xff })),
            lowerIrq: (irq) => enqueueIoEvent(encodeEvent({ kind: "irqLower", irq: irq & 0xff })),
          };

          const systemControl = {
            setA20: (enabled: boolean) => {
              enqueueIoEvent(encodeEvent({ kind: "a20Set", enabled: Boolean(enabled) }));
            },
            requestReset: () => {
              // Forward reset requests to the CPU side; the CPU worker will relay
              // this to the coordinator via the runtime event ring so the VM can
              // be reset/restarted.
              enqueueIoEvent(encodeEvent({ kind: "resetRequest" }));
            },
          };

          const serialSink: SerialOutputSink = {
            write: (port, data) => {
              // Serial output is emitted by the device model; forward it over
              // ioIpc so the CPU worker can decide how to surface it (console/UI,
              // log capture, etc).
              enqueueIoEvent(encodeEvent({ kind: "serialOutput", port: port & 0xffff, data }), { bestEffort: true });
            },
          };

          const mgr = new DeviceManager(irqSink);
          deviceManager = mgr;

          i8042 = new I8042Controller(mgr.irqSink, { systemControl });
          mgr.registerPortIo(0x0060, 0x0060, i8042);
          mgr.registerPortIo(0x0064, 0x0064, i8042);

          mgr.registerPciDevice(new PciTestDevice());

          const uart = new Uart16550(UART_COM1, serialSink);
          mgr.registerPortIo(uart.basePort, uart.basePort + 7, uart);

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
    const perfActive =
      !!perfWriter && !!perfFrameHeader && Atomics.load(perfFrameHeader, PERF_FRAME_HEADER_ENABLED_INDEX) !== 0;
    const t0 = perfActive ? performance.now() : 0;
    drainRuntimeCommands();
    drainIoIpcCommands();
    deviceManager?.tick(performance.now());
    if (perfActive) perfIoMs += performance.now() - t0;
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

    switch (cmd.kind) {
      case "diskRead":
        handleDiskRead(cmd);
        break;
      case "diskWrite":
        handleDiskWrite(cmd);
        break;
      case "portRead":
        handlePortRead(cmd);
        break;
      case "portWrite":
        handlePortWrite(cmd);
        break;
      case "mmioRead":
        handleMmioRead(cmd);
        break;
      case "mmioWrite":
        handleMmioWrite(cmd);
        break;
      case "nop":
        enqueueIoEvent(encodeEvent({ kind: "ack", seq: cmd.seq }));
        break;
      case "shutdown":
        Atomics.store(status, StatusIndex.StopRequested, 1);
        break;
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

function enqueueIoEvent(bytes: Uint8Array, opts?: { bestEffort?: boolean }): void {
  const evtRing = ioEvtRing;
  if (!evtRing) return;
  if (evtRing.tryPush(bytes)) return;
  if (opts?.bestEffort) return;
  pendingIoEvents.push(bytes);
}

function valueToLeBytes(value: number, size: number): Uint8Array {
  const out = new Uint8Array(size >>> 0);
  const v = value >>> 0;
  if (size >= 1) out[0] = v & 0xff;
  if (size >= 2) out[1] = (v >>> 8) & 0xff;
  if (size >= 3) out[2] = (v >>> 16) & 0xff;
  if (size >= 4) out[3] = (v >>> 24) & 0xff;
  return out;
}

function leBytesToU32(bytes: Uint8Array): number {
  const b0 = bytes[0] ?? 0;
  const b1 = bytes[1] ?? 0;
  const b2 = bytes[2] ?? 0;
  const b3 = bytes[3] ?? 0;
  return (b0 | (b1 << 8) | (b2 << 16) | (b3 << 24)) >>> 0;
}

function handlePortRead(cmd: Extract<Command, { kind: "portRead" }>): void {
  const id = cmd.id >>> 0;
  let value = 0;
  const mgr = deviceManager;
  if (mgr) {
    try {
      value = mgr.portRead(cmd.port, cmd.size);
    } catch {
      value = 0;
    }
  }

  portReadCount++;
  if ((portReadCount & 0xff) === 0) perf.counter("io:portReads", portReadCount);
  enqueueIoEvent(encodeEvent({ kind: "portReadResp", id, value: value >>> 0 }));
}

function handlePortWrite(cmd: Extract<Command, { kind: "portWrite" }>): void {
  const id = cmd.id >>> 0;
  const mgr = deviceManager;
  if (mgr) {
    try {
      mgr.portWrite(cmd.port, cmd.size, cmd.value);
    } catch {
      // Ignore device errors; still reply so the CPU side doesn't deadlock.
    }
  }

  portWriteCount++;
  if ((portWriteCount & 0xff) === 0) perf.counter("io:portWrites", portWriteCount);
  enqueueIoEvent(encodeEvent({ kind: "portWriteResp", id }));
}

function handleMmioRead(cmd: Extract<Command, { kind: "mmioRead" }>): void {
  const id = cmd.id >>> 0;
  let value = 0;
  const mgr = deviceManager;
  if (mgr) {
    try {
      value = mgr.mmioRead(cmd.addr, cmd.size);
    } catch {
      value = 0;
    }
  }

  mmioReadCount++;
  if ((mmioReadCount & 0xff) === 0) perf.counter("io:mmioReads", mmioReadCount);
  enqueueIoEvent(encodeEvent({ kind: "mmioReadResp", id, data: valueToLeBytes(value, cmd.size) }));
}

function handleMmioWrite(cmd: Extract<Command, { kind: "mmioWrite" }>): void {
  const id = cmd.id >>> 0;
  const value = leBytesToU32(cmd.data);
  const mgr = deviceManager;
  if (mgr) {
    try {
      mgr.mmioWrite(cmd.addr, cmd.data.byteLength, value);
    } catch {
      // Ignore device errors; still reply so the CPU side doesn't deadlock.
    }
  }

  mmioWriteCount++;
  if ((mmioWriteCount & 0xff) === 0) perf.counter("io:mmioWrites", mmioWriteCount);
  enqueueIoEvent(encodeEvent({ kind: "mmioWriteResp", id }));
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

  const remote = activeRemoteDisk;
  if (remote) {
    if (at + len > remote.capacityBytes) {
      enqueueIoEvent(encodeEvent({ kind: "diskReadResp", id, ok: false, bytes: 0, errorCode: DISK_ERROR_DISK_OOB }));
      return;
    }
    // Preserve request ordering. Some callers treat disk I/O as synchronous and
    // assume responses arrive in the same order as commands.
    remoteDiskReadChain = remoteDiskReadChain
      .catch(() => {
        // Keep queue alive after unexpected errors.
      })
      .then(async () => {
        try {
          await remote.readInto(at, view);
          perfIoReadBytes += len >>> 0;
          enqueueIoEvent(encodeEvent({ kind: "diskReadResp", id, ok: true, bytes: len >>> 0 }));
        } catch {
          enqueueIoEvent(
            encodeEvent({ kind: "diskReadResp", id, ok: false, bytes: 0, errorCode: DISK_ERROR_IO_FAILURE }),
          );
        }
      });
    return;
  }

  const handle = activeAccessHandle;
  if (!handle) {
    enqueueIoEvent(encodeEvent({ kind: "diskReadResp", id, ok: false, bytes: 0, errorCode: DISK_ERROR_NO_ACTIVE_DISK }));
    return;
  }
  const capacityBytes = activeDiskCapacityBytes;
  if (capacityBytes !== null && at + len > capacityBytes) {
    enqueueIoEvent(encodeEvent({ kind: "diskReadResp", id, ok: false, bytes: 0, errorCode: DISK_ERROR_DISK_OOB }));
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

  if (activeRemoteDisk) {
    enqueueIoEvent(encodeEvent({ kind: "diskWriteResp", id, ok: false, bytes: 0, errorCode: DISK_ERROR_READ_ONLY }));
    return;
  }

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
  const capacityBytes = activeDiskCapacityBytes;
  if (capacityBytes !== null && at + len > capacityBytes) {
    enqueueIoEvent(encodeEvent({ kind: "diskWriteResp", id, ok: false, bytes: 0, errorCode: DISK_ERROR_DISK_OOB }));
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
      case InputEventType.KeyScancode: {
        // Payload: a=packed bytes LE, b=len.
        const packed = words[off + 2] >>> 0;
        const len = words[off + 3] >>> 0;
        if (i8042) {
          const bytes = new Uint8Array(len);
          for (let j = 0; j < len; j++) {
            bytes[j] = (packed >>> (j * 8)) & 0xff;
          }
          i8042.injectKeyboardBytes(bytes);
        }
        break;
      }
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
  activeDiskCapacityBytes = null;
  void closeActiveRemoteDisk();
  usbHid?.free();
  usbHid = null;
  deviceManager = null;
  i8042 = null;
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
