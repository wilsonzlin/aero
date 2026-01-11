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
import type { MountConfig } from "../storage/metadata";
import { RuntimeDiskClient, type DiskImageMetadata } from "../storage/runtime_disk_client";
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
    const portHint = msg.guestPort === undefined ? "" : ` port=${msg.guestPort}`;
    this.host.log(
      `hid.attach deviceId=${msg.deviceId}${portHint} vid=0x${msg.vendorId.toString(16).padStart(4, "0")} pid=0x${msg.productId.toString(16).padStart(4, "0")}`,
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

type SetMicrophoneRingBufferMessage = {
  type: "setMicrophoneRingBuffer";
  ringBuffer: SharedArrayBuffer | null;
  /** Actual capture sample rate (AudioContext.sampleRate). */
  sampleRate?: number;
};

type SetBootDisksMessage = {
  type: "setBootDisks";
  mounts: MountConfig;
  hdd: DiskImageMetadata | null;
  cd: DiskImageMetadata | null;
};

type ActiveDisk = { handle: number; sectorSize: number; capacityBytes: number; readOnly: boolean };
let diskClient: RuntimeDiskClient | null = null;
let activeDisk: ActiveDisk | null = null;
let cdDisk: ActiveDisk | null = null;
let pendingBootDisks: SetBootDisksMessage | null = null;
let diskIoChain: Promise<void> = Promise.resolve();

let bootDisksInitResolve: (() => void) | null = null;
const bootDisksInitPromise = new Promise<void>((resolve) => {
  bootDisksInitResolve = resolve;
});

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

function queueDiskIo(op: () => Promise<void>): void {
  // Preserve ordering. Some callers treat disk I/O as synchronous and assume
  // responses arrive in the same order as commands.
  diskIoChain = diskIoChain
    .catch(() => {
      // Keep chain alive after unexpected errors.
    })
    .then(op)
    .catch((err) => {
      console.error(`[io.worker] disk I/O failed: ${err instanceof Error ? err.message : String(err)}`);
    });
}

function queueDiskIoResult(op: () => Promise<AeroIpcIoDiskResult>): Promise<AeroIpcIoDiskResult> {
  const chained = diskIoChain
    .catch(() => {
      // Keep chain alive after unexpected errors.
    })
    .then(op)
    .catch((err) => {
      console.error(`[io.worker] disk I/O failed: ${err instanceof Error ? err.message : String(err)}`);
      return { ok: false, bytes: 0, errorCode: DISK_ERROR_IO_FAILURE };
    });

  diskIoChain = chained.then(
    () => undefined,
    () => undefined,
  );
  return chained;
}

async function applyBootDisks(msg: SetBootDisksMessage): Promise<void> {
  if (!diskClient && !msg.hdd && !msg.cd) {
    activeDisk = null;
    cdDisk = null;
    return;
  }

  if (!diskClient) diskClient = new RuntimeDiskClient();
  const client = diskClient;

  // Close any existing handles first to avoid OPFS lock contention.
  const handles = new Set<number>();
  if (activeDisk) handles.add(activeDisk.handle);
  if (cdDisk) handles.add(cdDisk.handle);
  for (const handle of handles) {
    try {
      await client.flush(handle);
    } catch {
      // Ignore flush errors; close is best-effort during remount.
    }
    try {
      await client.closeDisk(handle);
    } catch {
      // Best-effort; continue.
    }
  }

  activeDisk = null;
  cdDisk = null;

  if (!msg.hdd && !msg.cd) return;

  let openedHdd: ActiveDisk | null = null;
  let openedCd: ActiveDisk | null = null;
  try {
    if (msg.hdd) {
      const res = await client.open(msg.hdd, { mode: "cow" });
      openedHdd = { handle: res.handle, sectorSize: res.sectorSize, capacityBytes: res.capacityBytes, readOnly: res.readOnly };
    }
    if (msg.cd) {
      const res = await client.open(msg.cd, { mode: "direct" });
      openedCd = { handle: res.handle, sectorSize: res.sectorSize, capacityBytes: res.capacityBytes, readOnly: res.readOnly };
    }
  } catch (err) {
    if (openedHdd) await client.closeDisk(openedHdd.handle).catch(() => undefined);
    if (openedCd) await client.closeDisk(openedCd.handle).catch(() => undefined);
    throw err;
  }

  activeDisk = openedHdd ?? openedCd;
  cdDisk = openedCd;
}

async function initWorker(init: WorkerInitMessage): Promise<void> {
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

      // Wait until the main thread provides the boot disk selection. This is required to
      // ensure the first diskRead issued by the CPU worker does not race with disk open.
      await bootDisksInitPromise;
      if (pendingBootDisks) {
        await applyBootDisks(pendingBootDisks);
      }

      pushEvent({ kind: "log", level: "info", message: "worker ready" });

      setReadyFlag(status, role, true);
      ctx.postMessage({ type: MessageType.READY, role } satisfies ProtocolMessage);
      if (perf.traceEnabled) perf.instant("boot:worker:ready", "p", { role });
    } finally {
      perf.spanEnd("worker:init");
    }
  } catch (err) {
    fatal(err);
    return;
  } finally {
    perf.spanEnd("worker:boot");
  }

  startIoIpcServer();
}

ctx.onmessage = (ev: MessageEvent<unknown>) => {
  try {
    const data = ev.data as
      | Partial<WorkerInitMessage>
      | Partial<ConfigUpdateMessage>
      | Partial<InputBatchMessage>
      | Partial<SetBootDisksMessage>
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

    if ((data as Partial<SetBootDisksMessage>).type === "setBootDisks") {
      const msg = data as Partial<SetBootDisksMessage>;
      pendingBootDisks = {
        type: "setBootDisks",
        mounts: (msg.mounts || {}) as MountConfig,
        hdd: (msg.hdd as DiskImageMetadata | null) ?? null,
        cd: (msg.cd as DiskImageMetadata | null) ?? null,
      };
      if (bootDisksInitResolve) {
        bootDisksInitResolve();
        bootDisksInitResolve = null;
      }
      if (started && pendingBootDisks) {
        queueDiskIo(() => applyBootDisks(pendingBootDisks!));
      }
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
      void initWorker(data as WorkerInitMessage);
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

function guestRangeView(guestOffset: bigint, len: number): Uint8Array | null {
  const guestBytes = BigInt(guestU8.byteLength);
  if (guestOffset < 0n) return null;
  const end = guestOffset + BigInt(len >>> 0);
  if (end > guestBytes) return null;
  const start = Number(guestOffset);
  return guestU8.subarray(start, start + (len >>> 0));
}

function computeAlignedDiskIoRange(
  diskOffset: bigint,
  lenU32: number,
  sectorSize: number,
): { lba: number; byteLength: number; offset: number } | null {
  if (sectorSize <= 0) return null;
  const sectorSizeBig = BigInt(sectorSize);
  const startLbaBig = diskOffset / sectorSizeBig;
  const offsetBig = diskOffset % sectorSizeBig;
  if (startLbaBig > BigInt(Number.MAX_SAFE_INTEGER)) return null;

  const endByte = diskOffset + BigInt(lenU32);
  const endLbaBig = lenU32 === 0 ? startLbaBig : (endByte + sectorSizeBig - 1n) / sectorSizeBig;
  const sectorsBig = endLbaBig - startLbaBig;
  const byteLengthBig = sectorsBig * sectorSizeBig;
  if (byteLengthBig > BigInt(Number.MAX_SAFE_INTEGER)) return null;

  return { lba: Number(startLbaBig), byteLength: Number(byteLengthBig), offset: Number(offsetBig) };
}

function diskRead(diskOffset: bigint, len: number, guestOffset: bigint): AeroIpcIoDiskResult | Promise<AeroIpcIoDiskResult> {
  const length = len >>> 0;

  const disk = activeDisk;
  const client = diskClient;
  if (!disk || !client) {
    return { ok: false, bytes: 0, errorCode: DISK_ERROR_NO_ACTIVE_DISK };
  }

  const view = guestRangeView(guestOffset, length);
  if (!view) {
    return { ok: false, bytes: 0, errorCode: DISK_ERROR_GUEST_OOB };
  }

  if (diskOffset < 0n || diskOffset + BigInt(length) > BigInt(disk.capacityBytes)) {
    return { ok: false, bytes: 0, errorCode: DISK_ERROR_DISK_OOB };
  }

  const range = computeAlignedDiskIoRange(diskOffset, length, disk.sectorSize);
  if (!range) {
    return { ok: false, bytes: 0, errorCode: DISK_ERROR_DISK_OFFSET_TOO_LARGE };
  }

  return queueDiskIoResult(async () => {
    const perfActive = isPerfActive();
    const t0 = perfActive ? performance.now() : 0;
    try {
      if (range.byteLength > 0) {
        const data = await client.read(disk.handle, range.lba, range.byteLength);
        view.set(data.subarray(range.offset, range.offset + length));
        perfIoReadBytes += data.byteLength;
      }
      return { ok: true, bytes: length };
    } catch {
      return { ok: false, bytes: 0, errorCode: DISK_ERROR_IO_FAILURE };
    } finally {
      if (perfActive) perfIoMs += performance.now() - t0;
    }
  });
}

function diskWrite(diskOffset: bigint, len: number, guestOffset: bigint): AeroIpcIoDiskResult | Promise<AeroIpcIoDiskResult> {
  const length = len >>> 0;

  const disk = activeDisk;
  const client = diskClient;
  if (!disk || !client) {
    return { ok: false, bytes: 0, errorCode: DISK_ERROR_NO_ACTIVE_DISK };
  }

  if (disk.readOnly) {
    return { ok: false, bytes: 0, errorCode: DISK_ERROR_READ_ONLY };
  }

  const view = guestRangeView(guestOffset, length);
  if (!view) {
    return { ok: false, bytes: 0, errorCode: DISK_ERROR_GUEST_OOB };
  }

  if (diskOffset < 0n || diskOffset + BigInt(length) > BigInt(disk.capacityBytes)) {
    return { ok: false, bytes: 0, errorCode: DISK_ERROR_DISK_OOB };
  }

  const range = computeAlignedDiskIoRange(diskOffset, length, disk.sectorSize);
  if (!range) {
    return { ok: false, bytes: 0, errorCode: DISK_ERROR_DISK_OFFSET_TOO_LARGE };
  }

  const aligned = range.offset === 0 && length % disk.sectorSize === 0;
  return queueDiskIoResult(async () => {
    const perfActive = isPerfActive();
    const t0 = perfActive ? performance.now() : 0;
    try {
      if (range.byteLength > 0) {
        if (aligned) {
          await client.write(disk.handle, range.lba, view);
          perfIoWriteBytes += length;
        } else {
          const buf = await client.read(disk.handle, range.lba, range.byteLength);
          buf.set(view, range.offset);
          perfIoReadBytes += buf.byteLength;
          await client.write(disk.handle, range.lba, buf);
          perfIoWriteBytes += buf.byteLength;
        }
      }
      return { ok: true, bytes: length };
    } catch {
      return { ok: false, bytes: 0, errorCode: DISK_ERROR_IO_FAILURE };
    } finally {
      if (perfActive) perfIoMs += performance.now() - t0;
    }
  });
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

  void (async () => {
    try {
      await diskIoChain.catch(() => undefined);
      if (diskClient) {
        const handles = new Set<number>();
        if (activeDisk) handles.add(activeDisk.handle);
        if (cdDisk) handles.add(cdDisk.handle);
        for (const handle of handles) {
          try {
            await diskClient.flush(handle);
          } catch {
            // Ignore flush errors during shutdown.
          }
          try {
            await diskClient.closeDisk(handle);
          } catch {
            // Ignore close errors during shutdown.
          }
        }
      }
    } finally {
      activeDisk = null;
      cdDisk = null;
      diskClient?.close();
      diskClient = null;

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
  })();
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
