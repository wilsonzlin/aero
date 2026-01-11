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
  IO_IPC_HID_IN_QUEUE_KIND,
  StatusIndex,
  createSharedMemoryViews,
  ringRegionsForWorker,
  setReadyFlag,
  type WorkerRole,
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
import { UhciPciDevice } from "../io/devices/uhci";
import { UART_COM1, Uart16550, type SerialOutputSink } from "../io/devices/uart16550";
import { AeroIpcIoServer, type AeroIpcIoDiskResult, type AeroIpcIoDispatchTarget } from "../io/ipc/aero_ipc_io";
import type { MountConfig } from "../storage/metadata";
import { RuntimeDiskClient, type DiskImageMetadata } from "../storage/runtime_disk_client";
import {
  isUsbRingAttachMessage,
  type UsbActionMessage,
  type UsbCompletionMessage,
  type UsbGuestWebUsbSnapshot,
  type UsbGuestWebUsbStatusMessage,
  type UsbHostAction,
  type UsbRingAttachMessage,
  type UsbSelectedMessage,
} from "../usb/usb_proxy_protocol";
import { applyUsbSelectedToWebUsbUhciBridge, type WebUsbUhciHotplugBridgeLike } from "../usb/uhci_webusb_bridge";
import type { UsbUhciHarnessStartMessage, UsbUhciHarnessStatusMessage, UsbUhciHarnessStopMessage, WebUsbUhciHarnessRuntimeSnapshot } from "../usb/webusb_harness_runtime";
import { WebUsbUhciHarnessRuntime } from "../usb/webusb_harness_runtime";
import { WebUsbPassthroughRuntime, type UsbPassthroughBridgeLike } from "../usb/webusb_passthrough_runtime";
import { UsbPassthroughDemoRuntime, type UsbPassthroughDemoResultMessage } from "../usb/usb_passthrough_demo_runtime";
import {
  isHidAttachMessage,
  isHidDetachMessage,
  isHidInputReportMessage,
  isHidRingAttachMessage,
  isHidRingInitMessage,
  type HidAttachMessage,
  type HidDetachMessage,
  type HidErrorMessage,
  type HidInputReportMessage,
  type HidLogMessage,
  type HidProxyMessage,
  type HidRingAttachMessage,
  type HidRingInitMessage,
  type HidSendReportMessage,
} from "../hid/hid_proxy_protocol";
import { UhciHidTopologyManager } from "../hid/uhci_hid_topology";
import { WasmHidGuestBridge, type HidHostSink } from "../hid/wasm_hid_guest_bridge";
import {
  isHidAttachHubMessage as isHidPassthroughAttachHubMessage,
  isHidAttachMessage as isHidPassthroughAttachMessage,
  isHidDetachMessage as isHidPassthroughDetachMessage,
  isHidInputReportMessage as isHidPassthroughInputReportMessage,
  type GuestUsbPath,
  type GuestUsbPort,
  type HidAttachMessage as HidPassthroughAttachMessage,
  type HidDetachMessage as HidPassthroughDetachMessage,
  type HidInputReportMessage as HidPassthroughInputReportMessage,
  type HidPassthroughMessage,
} from "../platform/hid_passthrough_protocol";
import { HidReportRing, HidReportType as HidRingReportType } from "../usb/hid_report_ring";
import { IoWorkerLegacyHidPassthroughAdapter } from "./io_hid_passthrough_legacy_adapter";
import { drainIoHidInputRing } from "./io_hid_input_ring";

const ctx = self as unknown as DedicatedWorkerGlobalScope;

void installWorkerPerfHandlers();

type InputBatchMessage = { type: "in:input-batch"; buffer: ArrayBuffer };
type InputBatchRecycleMessage = { type: "in:input-batch-recycle"; buffer: ArrayBuffer };

let role: WorkerRole = "io";
let status!: Int32Array;
let guestU8!: Uint8Array;
let guestBase = 0;
let guestSize = 0;

let commandRing!: RingBuffer;
let eventRing: RingBuffer | null = null;

let ioCmdRing: RingBuffer | null = null;
let ioEvtRing: RingBuffer | null = null;
let hidInRing: RingBuffer | null = null;
let hidProxyInputRing: RingBuffer | null = null;
let hidProxyInputRingForwarded = 0;
let hidProxyInputRingInvalid = 0;
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
let wasmApi: WasmApi | null = null;
let usbPassthroughRuntime: WebUsbPassthroughRuntime | null = null;
let usbPassthroughDebugTimer: number | undefined;
let usbUhciHarnessRuntime: WebUsbUhciHarnessRuntime | null = null;
let uhciDevice: UhciPciDevice | null = null;
type UhciControllerBridge = InstanceType<NonNullable<WasmApi["UhciControllerBridge"]>>;
let uhciControllerBridge: UhciControllerBridge | null = null;

type WebUsbGuestBridge = WebUsbUhciHotplugBridgeLike & UsbPassthroughBridgeLike;
let webUsbGuestBridge: WebUsbGuestBridge | null = null;
let lastUsbSelected: UsbSelectedMessage | null = null;
let usbRingAttach: UsbRingAttachMessage | null = null;

const WEBUSB_GUEST_ROOT_PORT = 1;

let webUsbGuestAttached = false;
let webUsbGuestLastError: string | null = null;
let lastWebUsbGuestSnapshot: UsbGuestWebUsbSnapshot | null = null;

function formatWebUsbGuestError(err: unknown): string {
  if (err instanceof Error) return err.message;
  return String(err);
}

function emitWebUsbGuestStatus(): void {
  const snapshot: UsbGuestWebUsbSnapshot = {
    available: webUsbGuestBridge !== null,
    attached: webUsbGuestAttached,
    blocked: !usbAvailable,
    rootPort: WEBUSB_GUEST_ROOT_PORT,
    lastError: webUsbGuestLastError,
  };

  const prev = lastWebUsbGuestSnapshot;
  if (
    prev &&
    prev.available === snapshot.available &&
    prev.attached === snapshot.attached &&
    prev.blocked === snapshot.blocked &&
    prev.rootPort === snapshot.rootPort &&
    prev.lastError === snapshot.lastError
  ) {
    return;
  }
  lastWebUsbGuestSnapshot = snapshot;

  // Routed through the main-thread UsbBroker so the WebUSB broker panel can display guest-visible attachment state.
  ctx.postMessage({ type: "usb.guest.status", snapshot } satisfies UsbGuestWebUsbStatusMessage);
}

const uhciHidTopology = new UhciHidTopologyManager();

function maybeInitUhciDevice(): void {
  const api = wasmApi;
  const mgr = deviceManager;
  if (!api || !mgr) return;
  if (!guestBase) return;

  if (!uhciDevice) {
    const Bridge = api.UhciControllerBridge;
    if (Bridge) {
      try {
        // `UhciControllerBridge` has multiple wasm-bindgen constructor signatures depending on
        // which WASM build is deployed:
        // - legacy: `new (guestBase)`
        // - current: `new (guestBase, guestSize)` (guestSize=0 means "use remainder of linear memory")
        //
        // wasm-bindgen glue sometimes enforces constructor arity, so pick based on `length` and
        // fall back to the other variant if instantiation fails.
        const base = guestBase >>> 0;
        const size = guestSize >>> 0;
        // eslint-disable-next-line @typescript-eslint/no-explicit-any
        const Ctor = Bridge as any;
        // eslint-disable-next-line @typescript-eslint/no-explicit-any
        let bridge: any;
        try {
          bridge = Ctor.length >= 2 ? new Ctor(base, size) : new Ctor(base);
        } catch {
          // Retry with the opposite arity to support older/newer wasm-bindgen outputs.
          bridge = Ctor.length >= 2 ? new Ctor(base) : new Ctor(base, size);
        }
        const dev = new UhciPciDevice({ bridge, irqSink: mgr.irqSink });
        uhciControllerBridge = bridge;
        uhciDevice = dev;
        mgr.registerPciDevice(dev);
        mgr.addTickable(dev);
        uhciHidTopology.setUhciBridge(bridge as unknown as any);
      } catch (err) {
        console.warn("[io.worker] Failed to initialize UHCI controller bridge", err);
      }
    }
  }

  if (!webUsbGuestBridge) {
    const bridge = uhciControllerBridge;
    const hasWebUsb =
      bridge &&
      typeof (bridge as unknown as { set_connected?: unknown }).set_connected === "function" &&
      typeof (bridge as unknown as { drain_actions?: unknown }).drain_actions === "function";

    if (bridge && hasWebUsb) {
      // `UhciPciDevice` owns the WASM bridge and calls `free()` during shutdown; wrap with a
      // no-op `free()` so `WebUsbPassthroughRuntime` does not double-free.
      const wrapped: WebUsbGuestBridge = {
        set_connected: (connected) => bridge.set_connected(connected),
        drain_actions: () => bridge.drain_actions(),
        push_completion: (completion) => bridge.push_completion(completion),
        reset: () => bridge.reset(),
        pending_summary: () => bridge.pending_summary(),
        free: () => {},
      };

      webUsbGuestBridge = wrapped;

      if (!usbPassthroughRuntime) {
        usbPassthroughRuntime = new WebUsbPassthroughRuntime({
          bridge: wrapped,
          port: ctx,
          pollIntervalMs: 0,
          initiallyBlocked: !usbAvailable,
          initialRingAttach: usbRingAttach ?? undefined,
        });
        usbPassthroughRuntime.start();
        if (import.meta.env.DEV) {
          usbPassthroughDebugTimer = setInterval(() => {
            console.debug("[io.worker] UHCI WebUSB pending_summary()", usbPassthroughRuntime?.pendingSummary());
          }, 1000) as unknown as number;
        }
      }

      if (lastUsbSelected) {
        try {
          applyUsbSelectedToWebUsbUhciBridge(wrapped, lastUsbSelected);
          webUsbGuestAttached = lastUsbSelected.ok;
          webUsbGuestLastError = null;
        } catch (err) {
          console.warn("[io.worker] Failed to apply usb.selected to guest WebUSB bridge", err);
          webUsbGuestAttached = false;
          webUsbGuestLastError = `Failed to apply usb.selected to guest WebUSB bridge: ${formatWebUsbGuestError(err)}`;
        }
      } else {
        webUsbGuestAttached = false;
        webUsbGuestLastError = null;
      }
    } else {
      webUsbGuestAttached = false;
      if (usbAvailable) {
        webUsbGuestLastError = bridge
          ? "UhciControllerBridge WebUSB passthrough exports unavailable (guest-visible WebUSB passthrough unsupported in this WASM build)."
          : "UhciControllerBridge unavailable (guest-visible WebUSB passthrough unsupported in this WASM build).";
      } else {
        webUsbGuestLastError = null;
      }
    }

    emitWebUsbGuestStatus();
  }
}

type UsbPassthroughDemo = InstanceType<NonNullable<WasmApi["UsbPassthroughDemo"]>>;
let usbDemo: UsbPassthroughDemoRuntime | null = null;
let usbDemoApi: UsbPassthroughDemo | null = null;

let hidInputRing: HidReportRing | null = null;
let hidOutputRing: HidReportRing | null = null;

function ensureArrayBufferBacked(bytes: Uint8Array): Uint8Array<ArrayBuffer> {
  // HID proxy messages transfer the underlying ArrayBuffer between threads.
  // If a view is backed by a SharedArrayBuffer, it can't be transferred; copy.
  if (bytes.buffer instanceof ArrayBuffer) return bytes as unknown as Uint8Array<ArrayBuffer>;
  const out = new Uint8Array(bytes.byteLength);
  out.set(bytes);
  return out;
}

function attachHidRings(msg: HidRingAttachMessage): void {
  // `isHidRingAttachMessage` validates SAB existence + instance checks.
  hidInputRing = new HidReportRing(msg.inputRing);
  hidOutputRing = new HidReportRing(msg.outputRing);
}

function drainHidInputRing(): void {
  const ring = hidInputRing;
  if (!ring) return;

  // eslint-disable-next-line no-constant-condition
  while (true) {
    const consumed = ring.consumeNext((rec) => {
      if (rec.reportType !== HidRingReportType.Input) return;
      if (started) Atomics.add(status, StatusIndex.IoHidInputReportCounter, 1);
      hidGuest.inputReport({
        type: "hid.inputReport",
        deviceId: rec.deviceId,
        reportId: rec.reportId,
        // Ring buffers are backed by SharedArrayBuffer; the WASM bridge accepts Uint8Array views regardless of buffer type.
        data: rec.payload as unknown as Uint8Array<ArrayBuffer>,
      });
    });
    if (!consumed) break;
  }
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
    const pathHint = msg.guestPath ? ` path=${msg.guestPath.join(".")}` : msg.guestPort === undefined ? "" : ` port=${msg.guestPort}`;
    this.host.log(
      `hid.attach deviceId=${msg.deviceId}${pathHint} vid=0x${msg.vendorId.toString(16).padStart(4, "0")} pid=0x${msg.productId.toString(16).padStart(4, "0")}`,
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
    // `HidInputReportMessage.data` is normally ArrayBuffer-backed because it's
    // transferred over postMessage. Some fast paths (SharedArrayBuffer rings)
    // can deliver views backed by SharedArrayBuffer; copy those so buffered
    // reports remain valid after the ring memory is reused.
    const data = ensureArrayBufferBacked(msg.data);
    queue.push({ ...msg, data });
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

const legacyHidAdapter = new IoWorkerLegacyHidPassthroughAdapter();

const hidHostSink: HidHostSink = {
  sendReport: (payload) => {
    const legacyMsg = legacyHidAdapter.sendReport(payload);
    if (legacyMsg) {
      ctx.postMessage(legacyMsg, [legacyMsg.data]);
      return;
    }

    const outRing = hidOutputRing;
    if (outRing) {
      const ty = payload.reportType === "feature" ? HidRingReportType.Feature : HidRingReportType.Output;
      outRing.push(payload.deviceId >>> 0, ty, payload.reportId >>> 0, payload.data);
      return;
    }

    const data = ensureArrayBufferBacked(payload.data);
    const msg: HidSendReportMessage = { type: "hid.sendReport", ...payload, data };
    ctx.postMessage(msg, [data.buffer]);
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
let wasmHidGuest: HidGuestBridge | null = null;

// -----------------------------------------------------------------------------
// WebHID passthrough (main thread ↔ I/O worker) debug plumbing
// -----------------------------------------------------------------------------

const hidPassthroughPathsByDeviceId = new Map<string, GuestUsbPath>();

type HidPassthroughInputReportDebugEntry = {
  reportId: number;
  byteLength: number;
  receivedAtMs: number;
  previewHex: string;
};

const HID_PASSTHROUGH_INPUT_REPORT_HISTORY_LIMIT = 32;
const HID_PASSTHROUGH_INPUT_REPORT_PREVIEW_BYTES = 16;
const HID_PASSTHROUGH_DEBUG_MAX_OUTPUT_REPORT_BYTES = 1024;

const hidPassthroughAttachById = new Map<string, HidPassthroughAttachMessage>();
const hidPassthroughInputReportHistoryById = new Map<string, HidPassthroughInputReportDebugEntry[]>();
const hidPassthroughInputReportCountById = new Map<string, number>();
const hidPassthroughDebugOutputRequested = new Set<string>();

type NormalizedHidCollection = HidPassthroughAttachMessage["collections"][number];
type NormalizedHidReportInfo = NormalizedHidCollection["outputReports"][number];

function formatHexPreview(buffer: ArrayBuffer, limit = HID_PASSTHROUGH_INPUT_REPORT_PREVIEW_BYTES): string {
  const bytes = new Uint8Array(buffer);
  const slice = bytes.byteLength > limit ? bytes.subarray(0, limit) : bytes;
  const hex = Array.from(slice, (b) => b.toString(16).padStart(2, "0")).join(" ");
  return bytes.byteLength > limit ? `${hex} …` : hex;
}

function estimateReportByteLength(report: NormalizedHidReportInfo): number {
  let bits = 0;
  for (const item of report.items) {
    bits += (item.reportSize >>> 0) * (item.reportCount >>> 0);
  }
  return Math.ceil(bits / 8);
}

function findFirstSendableReport(
  collections: readonly NormalizedHidCollection[],
): { reportType: "output" | "feature"; reportId: number; byteLength: number } | null {
  for (const col of collections) {
    const out = col.outputReports[0];
    if (out) {
      return { reportType: "output", reportId: out.reportId, byteLength: estimateReportByteLength(out) };
    }
    const feature = col.featureReports[0];
    if (feature) {
      return { reportType: "feature", reportId: feature.reportId, byteLength: estimateReportByteLength(feature) };
    }
    const nested = findFirstSendableReport(col.children);
    if (nested) return nested;
  }
  return null;
}

function handleHidPassthroughAttachHub(msg: Extract<HidPassthroughMessage, { type: "hid:attachHub" }>): void {
  uhciHidTopology.setHubConfig(msg.guestPath, msg.portCount);
  maybeInitUhciDevice();
  if (import.meta.env.DEV) {
    const hint = msg.portCount !== undefined ? ` ports=${msg.portCount}` : "";
    console.info(`[hid] attachHub path=${msg.guestPath.join(".")}${hint}`);
  }
}

function handleHidPassthroughAttach(msg: HidPassthroughAttachMessage): void {
  const guestPath = msg.guestPath ?? (msg.guestPort !== undefined ? [msg.guestPort as GuestUsbPort] : null);
  if (!guestPath) return;

  hidPassthroughPathsByDeviceId.set(msg.deviceId, guestPath);
  hidPassthroughAttachById.set(msg.deviceId, msg);
  hidPassthroughInputReportHistoryById.delete(msg.deviceId);
  hidPassthroughInputReportCountById.delete(msg.deviceId);
  hidPassthroughDebugOutputRequested.delete(msg.deviceId);

  if (import.meta.env.DEV) {
    console.info(
      `[hid] attach deviceId=${msg.deviceId} path=${guestPath.join(".")} vid=0x${msg.vendorId.toString(16).padStart(4, "0")} pid=0x${msg.productId
        .toString(16)
        .padStart(4, "0")}`,
    );
  }

  // Dev-only smoke: issue a best-effort output/feature report request so the
  // worker→main→device round trip is exercised even before the USB stack is wired up.
  if (import.meta.env.DEV && !hidPassthroughDebugOutputRequested.has(msg.deviceId)) {
    const report = findFirstSendableReport(msg.collections);
    if (!report) return;

    const byteLength = Math.min(HID_PASSTHROUGH_DEBUG_MAX_OUTPUT_REPORT_BYTES, report.byteLength);
    const payload = new Uint8Array(byteLength);
    const data = payload.buffer;
    hidPassthroughDebugOutputRequested.add(msg.deviceId);
    try {
      ctx.postMessage(
        {
          type: "hid:sendReport",
          deviceId: msg.deviceId,
          reportType: report.reportType,
          reportId: report.reportId,
          data,
        } satisfies Extract<HidPassthroughMessage, { type: "hid:sendReport" }>,
        [data],
      );
      console.info(
        `[hid] debug requested ${report.reportType} report deviceId=${msg.deviceId} reportId=${report.reportId} len=${byteLength}`,
      );
    } catch (err) {
      console.warn("[hid] debug sendReport request failed", err);
    }
  }
}

function handleHidPassthroughDetach(msg: HidPassthroughDetachMessage): void {
  hidPassthroughPathsByDeviceId.delete(msg.deviceId);
  hidPassthroughAttachById.delete(msg.deviceId);
  hidPassthroughInputReportHistoryById.delete(msg.deviceId);
  hidPassthroughInputReportCountById.delete(msg.deviceId);
  hidPassthroughDebugOutputRequested.delete(msg.deviceId);

  if (import.meta.env.DEV) {
    console.info(`[hid] detach deviceId=${msg.deviceId}`);
  }
}

function handleHidPassthroughInputReport(msg: HidPassthroughInputReportMessage): void {
  const count = (hidPassthroughInputReportCountById.get(msg.deviceId) ?? 0) + 1;
  hidPassthroughInputReportCountById.set(msg.deviceId, count);

  const entry: HidPassthroughInputReportDebugEntry = {
    reportId: msg.reportId,
    byteLength: msg.data.byteLength,
    receivedAtMs: performance.now(),
    previewHex: formatHexPreview(msg.data),
  };

  const history = hidPassthroughInputReportHistoryById.get(msg.deviceId) ?? [];
  history.push(entry);
  while (history.length > HID_PASSTHROUGH_INPUT_REPORT_HISTORY_LIMIT) history.shift();
  hidPassthroughInputReportHistoryById.set(msg.deviceId, history);

  if (import.meta.env.DEV && (count <= 3 || (count & 0x7f) === 0)) {
    console.debug(
      `[hid] inputReport deviceId=${msg.deviceId} reportId=${msg.reportId} bytes=${msg.data.byteLength} #${count} ${entry.previewHex}`,
    );
  }
}
let currentConfig: AeroConfig | null = null;
let currentConfigVersion = 0;

function maybeInitWasmHidGuestBridge(): void {
  if (wasmHidGuest) return;
  const api = wasmApi;
  if (!api) return;

  // Ensure guest-visible USB controllers are registered before wiring up WebHID devices. If we
  // initialize the bridge before the UHCI controller exists, devices would never be visible to the
  // guest OS (PCI hotplug isn't modeled yet).
  maybeInitUhciDevice();
  if (api.UhciControllerBridge && !uhciControllerBridge) return;

  try {
    wasmHidGuest = new WasmHidGuestBridge(api, hidHostSink, uhciHidTopology);
  } catch (err) {
    console.warn("[io.worker] Failed to initialize WebHID passthrough WASM bridge", err);
    return;
  }

  // Replay any HID messages that arrived before WASM finished initializing so the
  // guest bridge sees a consistent device + input report stream.
  for (const attach of hidGuestInMemory.devices.values()) {
    wasmHidGuest.attach(attach);
    const reports = hidGuestInMemory.inputReports.get(attach.deviceId) ?? [];
    for (const report of reports) {
      wasmHidGuest.inputReport(report);
    }
  }

  // After WASM is ready we no longer need to buffer every input report in JS.
  // Keeping the in-memory bridge in the hot path is useful for debugging in dev
  // mode, but it adds avoidable per-report allocations/copies in production.
  if (import.meta.env.DEV) {
    hidGuest = new CompositeHidGuestBridge([hidGuestInMemory, wasmHidGuest]);
  } else {
    hidGuest = wasmHidGuest;
    hidGuestInMemory.devices.clear();
    hidGuestInMemory.inputReports.clear();
  }
}

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

// Dev-only smoke-test requests use a high id range to avoid colliding with ids generated by
// the WASM-side `UsbPassthroughDevice` model.
let usbDemoNextId = 1_000_000_000;

let usbAvailable = false;

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
        wasmApi = api;
        usbHid = new api.UsbHidBridge();
        maybeInitUhciDevice();

        maybeInitWasmHidGuestBridge();
        if (api.UsbPassthroughDemo && !usbDemo) {
          try {
            usbDemoApi = new api.UsbPassthroughDemo();
            usbDemo = new UsbPassthroughDemoRuntime({
              demo: usbDemoApi,
              postMessage: (msg: UsbActionMessage | UsbPassthroughDemoResultMessage) => {
                ctx.postMessage(msg as unknown);
                if (import.meta.env.DEV && msg.type === "usb.demoResult") {
                  if (msg.result.status === "success") {
                    const bytes = msg.result.data;
                    const idVendor = bytes.length >= 10 ? bytes[8]! | (bytes[9]! << 8) : null;
                    const idProduct = bytes.length >= 12 ? bytes[10]! | (bytes[11]! << 8) : null;
                    console.log("[io.worker] WebUSB demo result ok", {
                      bytes: Array.from(bytes),
                      idVendor,
                      idProduct,
                    });
                  } else {
                    console.log("[io.worker] WebUSB demo result", msg.result);
                  }
                }
              },
            });

            if (lastUsbSelected) {
              usbDemo.onUsbSelected(lastUsbSelected);
            }
          } catch (err) {
            console.warn("[io.worker] Failed to initialize WebUSB passthrough demo", err);
            try {
              usbDemoApi?.free();
            } catch {
              // ignore
            }
            usbDemoApi = null;
            usbDemo = null;
          }
        }

        if (import.meta.env.DEV && api.WebUsbUhciPassthroughHarness && !usbUhciHarnessRuntime) {
          const ctor = api.WebUsbUhciPassthroughHarness;
          try {
            usbUhciHarnessRuntime = new WebUsbUhciHarnessRuntime({
              createHarness: () => new ctor(),
              port: ctx,
              initiallyBlocked: true,
              initialRingAttach: usbRingAttach ?? undefined,
              onUpdate: (snapshot) => {
                ctx.postMessage({ type: "usb.harness.status", snapshot } satisfies UsbUhciHarnessStatusMessage);
              },
            });
          } catch (err) {
            console.warn("[io.worker] Failed to initialize WebUSB UHCI harness runtime", err);
            usbUhciHarnessRuntime = null;
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
      guestBase = views.guestLayout.guest_base >>> 0;
      guestSize = views.guestLayout.guest_size >>> 0;
      const regions = ringRegionsForWorker(role);
      commandRing = new RingBuffer(segments.control, regions.command.byteOffset);
      eventRing = new RingBuffer(segments.control, regions.event.byteOffset);
      ioCmdRing = openRingByKind(segments.ioIpc, IO_IPC_CMD_QUEUE_KIND);
      ioEvtRing = openRingByKind(segments.ioIpc, IO_IPC_EVT_QUEUE_KIND);
      try {
        hidInRing = openRingByKind(segments.ioIpc, IO_IPC_HID_IN_QUEUE_KIND);
      } catch {
        hidInRing = null;
      }

      // PCI devices may share IRQ lines. The CPU worker consumes IRQ events as a single
      // level per line, so we need to "wire-OR" multiple devices in the IO worker.
      //
      // Use a small refcount per IRQ line: emit `irqRaise` on 0->1 and `irqLower` on 1->0.
      const irqRefCounts = new Uint16Array(256);
      const irqSink: IrqSink = {
        raiseIrq: (irq) => {
          const idx = irq & 0xff;
          const prev = irqRefCounts[idx]!;
          const next = prev + 1;
          irqRefCounts[idx] = next;
          if (prev === 0) enqueueIoEvent(encodeEvent({ kind: "irqRaise", irq: idx }));
        },
        lowerIrq: (irq) => {
          const idx = irq & 0xff;
          const prev = irqRefCounts[idx]!;
          if (prev === 0) return;
          const next = prev - 1;
          irqRefCounts[idx] = next;
          if (next === 0) enqueueIoEvent(encodeEvent({ kind: "irqLower", irq: idx }));
        },
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
      maybeInitUhciDevice();

      const uart = new Uart16550(UART_COM1, serialSink);
      mgr.registerPortIo(uart.basePort, uart.basePort + 7, uart);

      // If WASM has already finished initializing, install the WebHID passthrough bridge now that
      // we have a device manager (UHCI needs IRQ wiring + PCI registration).
      maybeInitWasmHidGuestBridge();

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
      | Partial<UsbUhciHarnessStartMessage>
      | Partial<UsbUhciHarnessStopMessage>
      | Partial<HidAttachMessage>
      | Partial<HidInputReportMessage>
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

    if ((data as Partial<UsbUhciHarnessStartMessage>).type === "usb.harness.start") {
      if (usbUhciHarnessRuntime) {
        usbUhciHarnessRuntime.start();
      } else {
        const snapshot: WebUsbUhciHarnessRuntimeSnapshot = {
          available: false,
          enabled: false,
          blocked: true,
          tickCount: 0,
          actionsForwarded: 0,
          completionsApplied: 0,
          pendingCompletions: 0,
          lastAction: null,
          lastCompletion: null,
          deviceDescriptor: null,
          configDescriptor: null,
          lastError: "WebUsbUhciPassthroughHarness export unavailable (or dev-only harness disabled).",
        };
        ctx.postMessage({ type: "usb.harness.status", snapshot } satisfies UsbUhciHarnessStatusMessage);
      }
      return;
    }

    if ((data as Partial<UsbUhciHarnessStopMessage>).type === "usb.harness.stop") {
      if (usbUhciHarnessRuntime) {
        usbUhciHarnessRuntime.stop();
      } else {
        const snapshot: WebUsbUhciHarnessRuntimeSnapshot = {
          available: false,
          enabled: false,
          blocked: true,
          tickCount: 0,
          actionsForwarded: 0,
          completionsApplied: 0,
          pendingCompletions: 0,
          lastAction: null,
          lastCompletion: null,
          deviceDescriptor: null,
          configDescriptor: null,
          lastError: null,
        };
        ctx.postMessage({ type: "usb.harness.status", snapshot } satisfies UsbUhciHarnessStatusMessage);
      }
      return;
    }

    if (isHidRingInitMessage(data)) {
      const msg = data as HidRingInitMessage;
      hidProxyInputRing = new RingBuffer(msg.sab, msg.offsetBytes);
      hidProxyInputRingForwarded = 0;
      hidProxyInputRingInvalid = 0;
      return;
    }

    if (isHidRingAttachMessage(data)) {
      attachHidRings(data);
      return;
    }

    if (isUsbRingAttachMessage(data)) {
      usbRingAttach = data;
      return;
    }

    if (isHidAttachMessage(data)) {
      if (started) Atomics.add(status, StatusIndex.IoHidAttachCounter, 1);
      hidGuest.attach(data);
      return;
    }

    if (isHidDetachMessage(data)) {
      if (started) Atomics.add(status, StatusIndex.IoHidDetachCounter, 1);
      hidGuest.detach(data);
      return;
    }

    if (isHidInputReportMessage(data)) {
      if (started) Atomics.add(status, StatusIndex.IoHidInputReportCounter, 1);
      hidGuest.inputReport(data);
      return;
    }

    if (isHidPassthroughAttachHubMessage(data)) {
      handleHidPassthroughAttachHub(data);
      return;
    }

    if (isHidPassthroughAttachMessage(data)) {
      handleHidPassthroughAttach(data);
      if (started) Atomics.add(status, StatusIndex.IoHidAttachCounter, 1);
      hidGuest.attach(legacyHidAdapter.attach(data));
      return;
    }

    if (isHidPassthroughDetachMessage(data)) {
      handleHidPassthroughDetach(data);
      const detach = legacyHidAdapter.detach(data);
      if (detach) {
        if (started) Atomics.add(status, StatusIndex.IoHidDetachCounter, 1);
        hidGuest.detach(detach);
      }
      return;
    }

    if (isHidPassthroughInputReportMessage(data)) {
      handleHidPassthroughInputReport(data);
      const input = legacyHidAdapter.inputReport(data);
      if (input) {
        if (started) Atomics.add(status, StatusIndex.IoHidInputReportCounter, 1);
        hidGuest.inputReport(input);
      }
      return;
    }

    if ((data as Partial<UsbSelectedMessage>).type === "usb.selected") {
      const msg = data as UsbSelectedMessage;
      usbAvailable = msg.ok;
      lastUsbSelected = msg;
      if (webUsbGuestBridge) {
        try {
          applyUsbSelectedToWebUsbUhciBridge(webUsbGuestBridge, msg);
          webUsbGuestAttached = msg.ok;
          webUsbGuestLastError = null;
        } catch (err) {
          console.warn("[io.worker] Failed to apply usb.selected to guest WebUSB bridge", err);
          webUsbGuestAttached = false;
          webUsbGuestLastError = `Failed to apply usb.selected to guest WebUSB bridge: ${formatWebUsbGuestError(err)}`;
        }
      } else {
        webUsbGuestAttached = false;
        if (!msg.ok) {
          webUsbGuestLastError = null;
        } else if (wasmApi && !wasmApi.UhciControllerBridge) {
          webUsbGuestLastError =
            "UhciControllerBridge export unavailable (guest-visible WebUSB passthrough unsupported in this WASM build).";
        } else {
          webUsbGuestLastError = null;
        }
      }
      usbDemo?.onUsbSelected(msg);
      emitWebUsbGuestStatus();

      // Dev-only smoke test: once a device is selected on the main thread, request the
      // first 18 bytes of the device descriptor to prove the cross-thread broker works.
      if (msg.ok && import.meta.env.DEV && !usbDemo) {
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
      usbDemo?.onUsbCompletion(msg);
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
      drainHidInputRing();
      const hidRing = hidInRing;
      if (hidRing) {
        const res = drainIoHidInputRing(hidRing, (msg) => hidGuest.inputReport(msg));
        if (res.forwarded > 0) {
          Atomics.add(status, StatusIndex.IoHidInputReportCounter, res.forwarded);
        }
        if (res.invalid > 0) {
          Atomics.add(status, StatusIndex.IoHidInputReportDropCounter, res.invalid);
        }
      }
      const proxyRing = hidProxyInputRing;
      if (proxyRing) {
        const res = drainIoHidInputRing(proxyRing, (msg) => hidGuest.inputReport(msg));
        if (res.forwarded > 0) {
          Atomics.add(status, StatusIndex.IoHidInputReportCounter, res.forwarded);
        }
        if (res.invalid > 0) {
          Atomics.add(status, StatusIndex.IoHidInputReportDropCounter, res.invalid);
        }
        hidProxyInputRingForwarded += res.forwarded;
        hidProxyInputRingInvalid += res.invalid;
        if (import.meta.env.DEV && (res.forwarded > 0 || res.invalid > 0) && (hidProxyInputRingForwarded & 0xff) === 0) {
          console.debug(
            `[io.worker] hid.ring.init drained forwarded=${hidProxyInputRingForwarded} invalid=${hidProxyInputRingInvalid}`,
          );
        }
      }
      mgr.tick(nowMs);
      hidGuest.poll?.();
      void usbPassthroughRuntime?.pollOnce();
      usbUhciHarnessRuntime?.pollOnce();
      usbDemo?.tick();
      usbDemo?.pollResults();

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

      webUsbGuestBridge = null;

      if (usbPassthroughRuntime) {
        usbPassthroughRuntime.destroy();
        usbPassthroughRuntime = null;
      }

      usbUhciHarnessRuntime?.destroy();
      usbUhciHarnessRuntime = null;
      uhciDevice?.destroy();
      uhciDevice = null;
      uhciControllerBridge = null;
      uhciHidTopology.setUhciBridge(null);
      try {
        usbDemoApi?.free();
      } catch {
        // ignore
      }
      usbDemoApi = null;
      usbDemo = null;
      lastUsbSelected = null;
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
