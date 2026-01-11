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
import { AeroIpcIoServer, type AeroIpcIoDiskResult, type AeroIpcIoDispatchTarget } from "../io/ipc/aero_ipc_io";
import { openSyncAccessHandleInDedicatedWorker } from "../platform/opfs.ts";
import { RemoteStreamingDisk, type RemoteDiskCacheStatus, type RemoteDiskTelemetrySnapshot } from "../platform/remote_disk";
import { DEFAULT_OPFS_DISK_IMAGES_DIRECTORY } from "../storage/disk_image_store";
import type { WorkerOpenToken } from "../storage/disk_image_store";
import type { UsbActionMessage, UsbCompletionMessage, UsbHostAction, UsbSelectedMessage } from "../usb/usb_proxy_protocol";
import { WebUsbPassthroughRuntime } from "../usb/webusb_passthrough_runtime";
import {
  isHidAttachMessage,
  isHidDetachMessage,
  isHidInputReportMessage,
  type HidAttachMessage,
  type HidDetachMessage,
  type HidErrorMessage,
  type HidInputReportMessage,
  type HidLogMessage,
  type HidProxyMessage,
  type HidSendReportMessage,
} from "../hid/hid_proxy_protocol";

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
let usbPassthroughRuntime: WebUsbPassthroughRuntime | null = null;
let usbPassthroughDebugTimer: number | undefined;

type HidHostSink = {
  sendReport: (msg: Omit<HidSendReportMessage, "type">) => void;
  log: (message: string, deviceId?: number) => void;
  error: (message: string, deviceId?: number) => void;
};

function ensureArrayBufferBacked(bytes: Uint8Array): Uint8Array<ArrayBuffer> {
  // HID proxy messages transfer the underlying ArrayBuffer between threads.
  // If a view is backed by a SharedArrayBuffer, it can't be transferred; copy.
  if (bytes.buffer instanceof ArrayBuffer) return bytes as unknown as Uint8Array<ArrayBuffer>;
  const out = new Uint8Array(bytes.byteLength);
  out.set(bytes);
  return out;
}

interface HidGuestBridge {
  attach(msg: HidAttachMessage): void;
  detach(msg: HidDetachMessage): void;
  inputReport(msg: HidInputReportMessage): void;
  poll?(): void;
  destroy?(): void;
}

const MAX_BUFFERED_HID_INPUT_REPORTS_PER_DEVICE = 256;

class InMemoryHidGuestBridge implements HidGuestBridge {
  readonly devices = new Map<number, HidAttachMessage>();
  readonly inputReports = new Map<number, HidInputReportMessage[]>();

  #inputCount = 0;

  constructor(private readonly host: HidHostSink) {}

  attach(msg: HidAttachMessage): void {
    this.devices.set(msg.deviceId, msg);
    // Treat (re-)attach as a new session; clear any buffered reports.
    this.inputReports.set(msg.deviceId, []);
    this.host.log(
      `hid.attach deviceId=${msg.deviceId} vid=0x${msg.vendorId.toString(16).padStart(4, "0")} pid=0x${msg.productId.toString(16).padStart(4, "0")}`,
      msg.deviceId,
    );
  }

  detach(msg: HidDetachMessage): void {
    this.devices.delete(msg.deviceId);
    this.inputReports.delete(msg.deviceId);
    this.host.log(`hid.detach deviceId=${msg.deviceId}`, msg.deviceId);
  }

  inputReport(msg: HidInputReportMessage): void {
    let queue = this.inputReports.get(msg.deviceId);
    if (!queue) {
      queue = [];
      this.inputReports.set(msg.deviceId, queue);
    }
    queue.push(msg);
    if (queue.length > MAX_BUFFERED_HID_INPUT_REPORTS_PER_DEVICE) {
      queue.splice(0, queue.length - MAX_BUFFERED_HID_INPUT_REPORTS_PER_DEVICE);
    }

    this.#inputCount += 1;
    if (import.meta.env.DEV && (this.#inputCount & 0xff) === 0) {
      this.host.log(
        `hid.inputReport deviceId=${msg.deviceId} reportId=${msg.reportId} bytes=${msg.data.byteLength}`,
        msg.deviceId,
      );
    }
  }
}

type WebHidPassthroughBridge = InstanceType<WasmApi["WebHidPassthroughBridge"]>;

class WasmHidGuestBridge implements HidGuestBridge {
  readonly #bridges = new Map<number, WebHidPassthroughBridge>();

  constructor(
    private readonly api: WasmApi,
    private readonly host: HidHostSink,
  ) {}

  attach(msg: HidAttachMessage): void {
    this.detach({ type: "hid.detach", deviceId: msg.deviceId });

    let bridge: WebHidPassthroughBridge;
    try {
      bridge = new this.api.WebHidPassthroughBridge(
        msg.vendorId,
        msg.productId,
        undefined,
        msg.productName,
        undefined,
        msg.collections,
      );
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      this.host.error(`Failed to construct WebHidPassthroughBridge: ${message}`, msg.deviceId);
      return;
    }

    this.#bridges.set(msg.deviceId, bridge);
  }

  detach(msg: HidDetachMessage): void {
    const existing = this.#bridges.get(msg.deviceId);
    if (!existing) return;
    this.#bridges.delete(msg.deviceId);
    try {
      existing.free();
    } catch {
      // ignore
    }
  }

  inputReport(msg: HidInputReportMessage): void {
    const bridge = this.#bridges.get(msg.deviceId);
    if (!bridge) return;
    try {
      bridge.push_input_report(msg.reportId, msg.data);
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      this.host.error(`WebHID push_input_report failed: ${message}`, msg.deviceId);
    }
  }

  poll(): void {
    for (const [deviceId, bridge] of this.#bridges) {
      let configured = false;
      try {
        configured = bridge.configured();
      } catch {
        configured = false;
      }
      if (!configured) continue;

      while (true) {
        let report: { reportType: "output" | "feature"; reportId: number; data: Uint8Array } | null = null;
        try {
          report = bridge.drain_next_output_report();
        } catch (err) {
          const message = err instanceof Error ? err.message : String(err);
          this.host.error(`drain_next_output_report failed: ${message}`, deviceId);
          break;
        }
        if (!report) break;

        this.host.sendReport({
          deviceId,
          reportType: report.reportType,
          reportId: report.reportId,
          data: ensureArrayBufferBacked(report.data),
        });
      }
    }
  }

  destroy(): void {
    for (const deviceId of Array.from(this.#bridges.keys())) {
      this.detach({ type: "hid.detach", deviceId });
    }
  }
}

class CompositeHidGuestBridge implements HidGuestBridge {
  constructor(private readonly sinks: HidGuestBridge[]) {}

  attach(msg: HidAttachMessage): void {
    for (const sink of this.sinks) sink.attach(msg);
  }

  detach(msg: HidDetachMessage): void {
    for (const sink of this.sinks) sink.detach(msg);
  }

  inputReport(msg: HidInputReportMessage): void {
    for (const sink of this.sinks) sink.inputReport(msg);
  }

  poll(): void {
    for (const sink of this.sinks) sink.poll?.();
  }

  destroy(): void {
    for (const sink of this.sinks) sink.destroy?.();
  }
}

const hidHostSink: HidHostSink = {
  sendReport: (payload) => {
    const msg: HidSendReportMessage = { type: "hid.sendReport", ...payload };
    ctx.postMessage(msg, [payload.data.buffer]);
  },
  log: (message, deviceId) => {
    const msg: HidLogMessage = { type: "hid.log", message, ...(deviceId !== undefined ? { deviceId } : {}) };
    ctx.postMessage(msg);
  },
  error: (message, deviceId) => {
    const msg: HidErrorMessage = { type: "hid.error", message, ...(deviceId !== undefined ? { deviceId } : {}) };
    ctx.postMessage(msg);
  },
};

const hidGuestInMemory = new InMemoryHidGuestBridge(hidHostSink);
let hidGuest: HidGuestBridge = hidGuestInMemory;

let currentConfig: AeroConfig | null = null;
let currentConfigVersion = 0;

let started = false;
let shuttingDown = false;
let ioServerAbort: AbortController | null = null;
let ioServerTask: Promise<void> | null = null;

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
  if (frameId === 0) {
    // Perf is enabled, but the main thread hasn't published a frame ID yet.
    // Keep accumulating so the first non-zero frame can include this interval.
    perfLastFrameId = 0;
    return;
  }
  if (perfLastFrameId === 0) {
    // First observed frame ID after enabling perf. Only emit if we have some
    // accumulated work; otherwise establish a baseline and wait for the next
    // frame boundary.
    if (perfIoMs <= 0 && perfIoReadBytes === 0 && perfIoWriteBytes === 0) {
      perfLastFrameId = frameId;
      return;
    }
  }
  if (frameId === perfLastFrameId) return;
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
      cacheLimitMiB === null
        ? null
        : typeof cacheLimitMiB === "number"
          ? cacheLimitMiB <= 0
            ? 0
            : cacheLimitMiB * 1024 * 1024
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
      | Partial<HidProxyMessage>
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

    if (isHidAttachMessage(data)) {
      hidGuest.attach(data);
      return;
    }

    if (isHidDetachMessage(data)) {
      hidGuest.detach(data);
      return;
    }

    if (isHidInputReportMessage(data)) {
      hidGuest.inputReport(data);
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
        if (msg.completion.status === "success" && "data" in msg.completion) {
          console.log("[io.worker] WebUSB completion success", msg.completion.kind, msg.completion.id, Array.from(msg.completion.data));
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

            try {
              const wasmHidGuest = new WasmHidGuestBridge(api, hidHostSink);
              // Replay any HID messages that arrived before WASM finished initializing so the
              // guest bridge sees a consistent device + input report stream.
              for (const attach of hidGuestInMemory.devices.values()) {
                wasmHidGuest.attach(attach);
                const reports = hidGuestInMemory.inputReports.get(attach.deviceId) ?? [];
                for (const report of reports) {
                  wasmHidGuest.inputReport(report);
                }
              }

              hidGuest = new CompositeHidGuestBridge([hidGuestInMemory, wasmHidGuest]);
            } catch (err) {
              console.warn("[io.worker] Failed to initialize WebHID passthrough WASM bridge", err);
            }

            if (import.meta.env.DEV && api.UsbPassthroughBridge && !usbPassthroughRuntime) {
              try {
                const bridge = new api.UsbPassthroughBridge();
                usbPassthroughRuntime = new WebUsbPassthroughRuntime({ bridge, port: ctx, pollIntervalMs: 8 });
                usbPassthroughRuntime.start();
                usbPassthroughDebugTimer = setInterval(() => {
                  console.debug("[io.worker] UsbPassthroughBridge pending_summary()", usbPassthroughRuntime?.pendingSummary());
                }, 1000) as unknown as number;
              } catch (err) {
                console.warn("[io.worker] Failed to initialize WebUSB passthrough runtime", err);
              }
            }
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

      startIoIpcServer();
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

function isPerfActive(): boolean {
  const header = perfFrameHeader;
  return !!perfWriter && !!header && Atomics.load(header, PERF_FRAME_HEADER_ENABLED_INDEX) !== 0;
}

function startIoIpcServer(): void {
  if (started) return;
  const cmdRing = ioCmdRing;
  const evtRing = ioEvtRing;
  const mgr = deviceManager;
  if (!cmdRing || !evtRing || !mgr) {
    throw new Error("I/O IPC rings are unavailable; worker was not initialized correctly.");
  }

  started = true;
  ioServerAbort = new AbortController();

  const dispatchTarget: AeroIpcIoDispatchTarget = {
    portRead: (port, size) => {
      let value = 0;
      try {
        value = mgr.portRead(port, size);
      } catch {
        value = 0;
      }
      portReadCount++;
      if ((portReadCount & 0xff) === 0) perf.counter("io:portReads", portReadCount);
      return value >>> 0;
    },
    portWrite: (port, size, value) => {
      try {
        mgr.portWrite(port, size, value);
      } catch {
        // Ignore device errors; still reply so the CPU side doesn't deadlock.
      }
      portWriteCount++;
      if ((portWriteCount & 0xff) === 0) perf.counter("io:portWrites", portWriteCount);
    },
    mmioRead: (addr, size) => {
      let value = 0;
      try {
        value = mgr.mmioRead(addr, size);
      } catch {
        value = 0;
      }
      mmioReadCount++;
      if ((mmioReadCount & 0xff) === 0) perf.counter("io:mmioReads", mmioReadCount);
      return value >>> 0;
    },
    mmioWrite: (addr, size, value) => {
      try {
        mgr.mmioWrite(addr, size, value);
      } catch {
        // Ignore device errors; still reply so the CPU side doesn't deadlock.
      }
      mmioWriteCount++;
      if ((mmioWriteCount & 0xff) === 0) perf.counter("io:mmioWrites", mmioWriteCount);
    },
    diskRead,
    diskWrite,
    tick: (nowMs) => {
      const perfActive = isPerfActive();
      const t0 = perfActive ? performance.now() : 0;

      flushPendingIoEvents();
      drainRuntimeCommands();
      mgr.tick(nowMs);
      hidGuest.poll?.();

      if (perfActive) perfIoMs += performance.now() - t0;
      maybeEmitPerfSample();

      if (Atomics.load(status, StatusIndex.StopRequested) === 1) {
        ioServerAbort?.abort();
      }
    },
  };

  const server = new AeroIpcIoServer(cmdRing, evtRing, dispatchTarget, {
    tickIntervalMs: 8,
    emitEvent: (bytes) => enqueueIoEvent(bytes),
  });

  ioServerTask = (async () => {
    try {
      await server.runAsync({ signal: ioServerAbort!.signal, yieldEveryNCommands: 128 });
    } catch (err) {
      fatal(err);
      return;
    }

    // A `shutdown` command on the ioIpc ring (or an abort) should tear down the
    // whole worker.
    try {
      Atomics.store(status, StatusIndex.StopRequested, 1);
    } catch {
      // ignore if status isn't initialized yet.
    }
    shutdown();
  })();
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
      ioServerAbort?.abort();
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
  flushPendingIoEvents();
  if (pendingIoEvents.length > 0) {
    // Preserve ordering: do not allow newer events to overtake buffered ones.
    if (opts?.bestEffort) return;
    pendingIoEvents.push(bytes);
    return;
  }
  if (evtRing.tryPush(bytes)) return;
  if (opts?.bestEffort) return;
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

function diskRead(diskOffset: bigint, len: number, guestOffset: bigint): AeroIpcIoDiskResult | Promise<AeroIpcIoDiskResult> {
  const length = len >>> 0;

  const view = guestRangeView(guestOffset, length);
  if (!view) {
    return { ok: false, bytes: 0, errorCode: DISK_ERROR_GUEST_OOB };
  }

  const at = diskOffsetToJsNumber(diskOffset, length);
  if (at === null) {
    return { ok: false, bytes: 0, errorCode: DISK_ERROR_DISK_OFFSET_TOO_LARGE };
  }

  const remote = activeRemoteDisk;
  if (remote) {
    if (at + length > remote.capacityBytes) {
      return { ok: false, bytes: 0, errorCode: DISK_ERROR_DISK_OOB };
    }
    // Preserve request ordering. Some callers treat disk I/O as synchronous and
    // assume responses arrive in the same order as commands.
    const op = remoteDiskReadChain
      .catch(() => {
        // Keep queue alive after unexpected errors.
      })
      .then(async () => {
        try {
          await remote.readInto(at, view);
          perfIoReadBytes += length;
          return { ok: true, bytes: length };
        } catch {
          return { ok: false, bytes: 0, errorCode: DISK_ERROR_IO_FAILURE };
        }
      });

    remoteDiskReadChain = op.then(
      () => undefined,
      () => undefined,
    );
    return op;
  }

  const handle = activeAccessHandle;
  if (!handle) {
    return { ok: false, bytes: 0, errorCode: DISK_ERROR_NO_ACTIVE_DISK };
  }
  const capacityBytes = activeDiskCapacityBytes;
  if (capacityBytes !== null && at + length > capacityBytes) {
    return { ok: false, bytes: 0, errorCode: DISK_ERROR_DISK_OOB };
  }

  try {
    const bytes = handle.read(view, { at });
    perfIoReadBytes += bytes >>> 0;
    return { ok: true, bytes: bytes >>> 0 };
  } catch {
    return { ok: false, bytes: 0, errorCode: DISK_ERROR_IO_FAILURE };
  }
}

function diskWrite(
  diskOffset: bigint,
  len: number,
  guestOffset: bigint,
): AeroIpcIoDiskResult | Promise<AeroIpcIoDiskResult> {
  const length = len >>> 0;
  if (activeRemoteDisk) {
    return { ok: false, bytes: 0, errorCode: DISK_ERROR_READ_ONLY };
  }

  const handle = activeAccessHandle;
  if (!handle) {
    return { ok: false, bytes: 0, errorCode: DISK_ERROR_NO_ACTIVE_DISK };
  }

  const view = guestRangeView(guestOffset, length);
  if (!view) {
    return { ok: false, bytes: 0, errorCode: DISK_ERROR_GUEST_OOB };
  }

  const at = diskOffsetToJsNumber(diskOffset, length);
  if (at === null) {
    return { ok: false, bytes: 0, errorCode: DISK_ERROR_DISK_OFFSET_TOO_LARGE };
  }
  const capacityBytes = activeDiskCapacityBytes;
  if (capacityBytes !== null && at + length > capacityBytes) {
    return { ok: false, bytes: 0, errorCode: DISK_ERROR_DISK_OOB };
  }

  try {
    const bytes = handle.write(view, { at });
    perfIoWriteBytes += bytes >>> 0;
    return { ok: true, bytes: bytes >>> 0 };
  } catch {
    return { ok: false, bytes: 0, errorCode: DISK_ERROR_IO_FAILURE };
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
  if (shuttingDown) return;
  shuttingDown = true;
  ioServerAbort?.abort();
  if (usbPassthroughDebugTimer !== undefined) {
    clearInterval(usbPassthroughDebugTimer);
    usbPassthroughDebugTimer = undefined;
  }

  hidGuest.destroy?.();

  activeAccessHandle?.close();
  activeDiskCapacityBytes = null;
  void closeActiveRemoteDisk();
  usbHid?.free();
  usbHid = null;
  usbPassthroughRuntime?.destroy();
  usbPassthroughRuntime = null;
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
  ioServerAbort?.abort();
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
