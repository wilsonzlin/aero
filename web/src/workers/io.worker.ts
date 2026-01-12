/// <reference lib="webworker" />

import type { AeroConfig } from "../config/aero_config";
import { openRingByKind } from "../ipc/ipc";
import { decodeCommand, encodeEvent, type Command, type Event } from "../ipc/protocol";
import { RingBuffer } from "../ipc/ring_buffer";
import { InputEventType } from "../input/event_queue";
import { chooseKeyboardInputBackend, chooseMouseInputBackend, type InputBackend } from "../input/input_backend_selection";
import { perf } from "../perf/perf";
import { installWorkerPerfHandlers } from "../perf/worker";
import { PerfWriter } from "../perf/writer.js";
import { PERF_FRAME_HEADER_ENABLED_INDEX, PERF_FRAME_HEADER_FRAME_ID_INDEX } from "../perf/shared.js";
import { initWasmForContext, type WasmApi, type WasmVariant } from "../runtime/wasm_context";
import { assertWasmMemoryWiring, WasmMemoryWiringError } from "../runtime/wasm_memory_probe";
import {
  serializeVmSnapshotError,
  type CoordinatorToWorkerSnapshotMessage,
  type VmSnapshotDeviceBlob,
  type VmSnapshotPausedMessage,
  type VmSnapshotResumedMessage,
  type VmSnapshotRestoredMessage,
  type VmSnapshotSavedMessage,
} from "../runtime/snapshot_protocol";
import {
  IO_IPC_CMD_QUEUE_KIND,
  IO_IPC_EVT_QUEUE_KIND,
  IO_IPC_HID_IN_QUEUE_KIND,
  IO_IPC_NET_RX_QUEUE_KIND,
  IO_IPC_NET_TX_QUEUE_KIND,
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
import {
  IRQ_REFCOUNT_ASSERT,
  IRQ_REFCOUNT_DEASSERT,
  IRQ_REFCOUNT_SATURATED,
  IRQ_REFCOUNT_UNDERFLOW,
  applyIrqRefCountChange,
} from "../io/irq_refcount";
import { I8042Controller } from "../io/devices/i8042";
import { E1000PciDevice } from "../io/devices/e1000";
import { HdaPciDevice, type HdaControllerBridgeLike } from "../io/devices/hda";
import { PciTestDevice } from "../io/devices/pci_test_device";
import { UhciPciDevice, type UhciControllerBridgeLike } from "../io/devices/uhci";
import { VirtioInputPciFunction, hidUsageToLinuxKeyCode, type VirtioInputPciDeviceLike } from "../io/devices/virtio_input";
import { VirtioNetPciDevice } from "../io/devices/virtio_net";
import { UART_COM1, Uart16550, type SerialOutputSink } from "../io/devices/uart16550";
import { AeroIpcIoServer, type AeroIpcIoDiskResult, type AeroIpcIoDispatchTarget } from "../io/ipc/aero_ipc_io";
import { defaultReadValue } from "../io/ipc/io_protocol";
import type { MountConfig } from "../storage/metadata";
import { RuntimeDiskClient, type DiskImageMetadata } from "../storage/runtime_disk_client";
import {
  isUsbRingAttachMessage,
  isUsbRingDetachMessage,
  isUsbCompletionMessage,
  isUsbSelectedMessage,
  type UsbActionMessage,
  type UsbCompletionMessage,
  type UsbGuestWebUsbSnapshot,
  type UsbGuestWebUsbStatusMessage,
  type UsbHostAction,
  type UsbHostCompletion,
  type UsbRingAttachMessage,
  type UsbRingDetachMessage,
  type UsbSelectedMessage,
} from "../usb/usb_proxy_protocol";
import { setUsbProxyCompletionRingDispatchPaused } from "../usb/usb_proxy_ring_dispatcher";
import { applyUsbSelectedToWebUsbUhciBridge, type WebUsbUhciHotplugBridgeLike } from "../usb/uhci_webusb_bridge";
import type { UsbUhciHarnessStartMessage, UsbUhciHarnessStatusMessage, UsbUhciHarnessStopMessage, WebUsbUhciHarnessRuntimeSnapshot } from "../usb/webusb_harness_runtime";
import { WebUsbUhciHarnessRuntime } from "../usb/webusb_harness_runtime";
import { WebUsbPassthroughRuntime, type UsbPassthroughBridgeLike } from "../usb/webusb_passthrough_runtime";
import { hex16 } from "../usb/usb_hex";
import {
  UsbPassthroughDemoRuntime,
  isUsbPassthroughDemoRunMessage,
  type UsbPassthroughDemoResultMessage,
  type UsbPassthroughDemoRunMessage,
} from "../usb/usb_passthrough_demo_runtime";
import {
  isHidAttachMessage,
  isHidDetachMessage,
  isHidInputReportMessage,
  isHidProxyMessage,
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
import { InMemoryHidGuestBridge, ensureArrayBufferBacked } from "../hid/in_memory_hid_guest_bridge";
import { UhciHidTopologyManager } from "../hid/uhci_hid_topology";
import { WasmHidGuestBridge, type HidGuestBridge, type HidHostSink } from "../hid/wasm_hid_guest_bridge";
import { WasmUhciHidGuestBridge } from "../hid/wasm_uhci_hid_guest_bridge";
import {
  HEADER_BYTES as AUDIO_OUT_HEADER_BYTES,
  getRingBufferLevelFrames as getAudioOutRingBufferLevelFrames,
  wrapRingBuffer as wrapAudioOutRingBuffer,
  type AudioWorkletRingBufferViews,
} from "../audio/audio_worklet_ring";
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
import {
  USB_HID_BOOT_KEYBOARD_REPORT_DESCRIPTOR,
  USB_HID_BOOT_MOUSE_REPORT_DESCRIPTOR,
  USB_HID_GAMEPAD_REPORT_DESCRIPTOR,
  USB_HID_INTERFACE_PROTOCOL_KEYBOARD,
  USB_HID_INTERFACE_PROTOCOL_MOUSE,
  USB_HID_INTERFACE_SUBCLASS_BOOT,
} from "../usb/hid_descriptors";
import {
  EXTERNAL_HUB_ROOT_PORT,
  WEBUSB_GUEST_ROOT_PORT,
  UHCI_SYNTHETIC_HID_GAMEPAD_HUB_PORT,
  UHCI_SYNTHETIC_HID_KEYBOARD_HUB_PORT,
  UHCI_SYNTHETIC_HID_MOUSE_HUB_PORT,
} from "../usb/uhci_external_hub";
import { IoWorkerLegacyHidPassthroughAdapter } from "./io_hid_passthrough_legacy_adapter";
import { drainIoHidInputRing } from "./io_hid_input_ring";
import { UhciRuntimeExternalHubConfigManager } from "./uhci_runtime_hub_config";
import {
  VM_SNAPSHOT_DEVICE_AUDIO_HDA_KIND,
  VM_SNAPSHOT_DEVICE_E1000_KIND,
  VM_SNAPSHOT_DEVICE_I8042_KIND,
  VM_SNAPSHOT_DEVICE_USB_KIND,
  parseAeroIoSnapshotVersion,
  resolveVmSnapshotRestoreFromOpfsExport,
  resolveVmSnapshotSaveToOpfsExport,
  vmSnapshotDeviceIdToKind,
  vmSnapshotDeviceKindToId,
} from "./vm_snapshot_wasm";
import { tryInitVirtioNetDevice } from "./io_virtio_net_init";
import { registerVirtioInputKeyboardPciFunction } from "./io_virtio_input_register";
import { VmTimebase } from "../runtime/vm_timebase";

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
let netTxRing: RingBuffer | null = null;
let netRxRing: RingBuffer | null = null;
let ioIpcSab: SharedArrayBuffer | null = null;
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

type I8042Bridge = InstanceType<NonNullable<WasmApi["I8042Bridge"]>>;

class I8042WasmController {
  readonly #bridge: I8042Bridge;
  readonly #irq: IrqSink;
  readonly #systemControl: { setA20(enabled: boolean): void; requestReset(): void };

  #irqMask = 0;
  #a20Enabled = false;

  constructor(bridge: I8042Bridge, irq: IrqSink, systemControl: { setA20(enabled: boolean): void; requestReset(): void }) {
    this.#bridge = bridge;
    this.#irq = irq;
    this.#systemControl = systemControl;
    this.#syncSideEffects();
  }

  free(): void {
    try {
      this.#bridge.free();
    } catch {
      // ignore
    }
  }

  portRead(port: number, size: number): number {
    if (size !== 1) return defaultReadValue(size);
    const value = this.#bridge.port_read(port & 0xffff) & 0xff;
    this.#syncSideEffects();
    return value;
  }

  portWrite(port: number, size: number, value: number): void {
    if (size !== 1) return;
    this.#bridge.port_write(port & 0xffff, value & 0xff);
    this.#syncSideEffects();
  }

  injectKeyScancode(packed: number, len: number): void {
    this.#bridge.inject_key_scancode_bytes(packed >>> 0, len & 0xff);
    this.#syncSideEffects();
  }

  injectMouseMove(dx: number, dyPs2: number): void {
    this.#bridge.inject_mouse_move(dx | 0, dyPs2 | 0);
    this.#syncSideEffects();
  }

  injectMouseButtons(buttons: number): void {
    this.#bridge.inject_mouse_buttons(buttons & 0xff);
    this.#syncSideEffects();
  }

  injectMouseWheel(delta: number): void {
    this.#bridge.inject_mouse_wheel(delta | 0);
    this.#syncSideEffects();
  }

  save_state(): Uint8Array {
    return this.#bridge.save_state();
  }

  load_state(bytes: Uint8Array): void {
    this.#bridge.load_state(bytes);
    this.#syncSideEffects();
  }

  #syncSideEffects(): void {
    this.#syncIrqs();
    this.#syncSystemControl();
  }

  #syncIrqs(): void {
    const next = this.#bridge.irq_mask() & 0xff;
    const prev = this.#irqMask;
    if (next === prev) return;
    this.#irqMask = next;

    // bit0: IRQ1, bit1: IRQ12
    const changes = (prev ^ next) & 0x03;
    if (changes & 0x01) {
      if (next & 0x01) this.#irq.raiseIrq(1);
      else this.#irq.lowerIrq(1);
    }
    if (changes & 0x02) {
      if (next & 0x02) this.#irq.raiseIrq(12);
      else this.#irq.lowerIrq(12);
    }
  }

  #syncSystemControl(): void {
    // wasm-bindgen getters may appear as either JS properties or no-arg methods depending on the
    // generated glue version. Accept both.
    let nextA20 = false;
    try {
      const raw = (this.#bridge as unknown as { a20_enabled?: unknown }).a20_enabled;
      if (typeof raw === "function") {
        nextA20 = Boolean((raw as (...args: unknown[]) => unknown).call(this.#bridge));
      } else {
        nextA20 = Boolean(raw);
      }
    } catch {
      nextA20 = false;
    }
    if (nextA20 !== this.#a20Enabled) {
      this.#a20Enabled = nextA20;
      this.#systemControl.setA20(nextA20);
    }

    const resets = this.#bridge.take_reset_requests() >>> 0;
    if (resets) {
      for (let i = 0; i < resets; i++) {
        this.#systemControl.requestReset();
      }
    }
  }
}

let i8042Ts: I8042Controller | null = null;
let i8042Wasm: I8042WasmController | null = null;

let portReadCount = 0;
let portWriteCount = 0;
let mmioReadCount = 0;
let mmioWriteCount = 0;

type UsbHidBridge = InstanceType<WasmApi["UsbHidBridge"]>;
let usbHid: UsbHidBridge | null = null;
type UsbHidPassthroughBridge = InstanceType<NonNullable<WasmApi["UsbHidPassthroughBridge"]>>;
let syntheticUsbKeyboard: UsbHidPassthroughBridge | null = null;
let syntheticUsbMouse: UsbHidPassthroughBridge | null = null;
let syntheticUsbGamepad: UsbHidPassthroughBridge | null = null;
let syntheticUsbHidAttached = false;
let syntheticUsbKeyboardPendingReport: Uint8Array | null = null;
let syntheticUsbGamepadPendingReport: Uint8Array | null = null;
let keyboardInputBackend: InputBackend = "ps2";
const pressedKeyboardHidUsages = new Uint8Array(256);
let pressedKeyboardHidUsageCount = 0;
let mouseInputBackend: InputBackend = "ps2";
let mouseButtonsMask = 0;
let wasmApi: WasmApi | null = null;
let usbPassthroughRuntime: WebUsbPassthroughRuntime | null = null;
let usbPassthroughDebugTimer: number | undefined;
let usbUhciHarnessRuntime: WebUsbUhciHarnessRuntime | null = null;
let uhciDevice: UhciPciDevice | null = null;
let virtioNetDevice: VirtioNetPciDevice | null = null;
type UhciControllerBridge = InstanceType<NonNullable<WasmApi["UhciControllerBridge"]>>;
let uhciControllerBridge: UhciControllerBridge | null = null;

let e1000Device: E1000PciDevice | null = null;
type E1000Bridge = InstanceType<NonNullable<WasmApi["E1000Bridge"]>>;
let e1000Bridge: E1000Bridge | null = null;

let hdaDevice: HdaPciDevice | null = null;
type HdaControllerBridge = InstanceType<NonNullable<WasmApi["HdaControllerBridge"]>>;
let hdaControllerBridge: HdaControllerBridge | null = null;

type VirtioInputPciDevice = VirtioInputPciDeviceLike;
let virtioInputKeyboard: VirtioInputPciFunction | null = null;
let virtioInputMouse: VirtioInputPciFunction | null = null;

type WebUsbGuestBridge = WebUsbUhciHotplugBridgeLike & UsbPassthroughBridgeLike;
let webUsbGuestBridge: WebUsbGuestBridge | null = null;
let lastUsbSelected: UsbSelectedMessage | null = null;
let usbRingAttach: UsbRingAttachMessage | null = null;

type UhciRuntimeCtor = NonNullable<WasmApi["UhciRuntime"]>;
type UhciRuntimeInstance = InstanceType<UhciRuntimeCtor>;
let uhciRuntime: UhciRuntimeInstance | null = null;
let uhciRuntimeWebUsbBridge: UhciRuntimeWebUsbBridge | null = null;
let uhciRuntimeHidGuest: WasmUhciHidGuestBridge | null = null;
const uhciRuntimeHubConfig = new UhciRuntimeExternalHubConfigManager();

let pendingWasmInit: { api: WasmApi; variant: WasmVariant } | null = null;
let wasmReadySent = false;

const SYNTHETIC_USB_HID_KEYBOARD_DEVICE_ID = 0x1000_0001;
const SYNTHETIC_USB_HID_MOUSE_DEVICE_ID = 0x1000_0002;
const SYNTHETIC_USB_HID_GAMEPAD_DEVICE_ID = 0x1000_0003;
const SYNTHETIC_USB_HID_KEYBOARD_PATH: GuestUsbPath = [EXTERNAL_HUB_ROOT_PORT, UHCI_SYNTHETIC_HID_KEYBOARD_HUB_PORT];
const SYNTHETIC_USB_HID_MOUSE_PATH: GuestUsbPath = [EXTERNAL_HUB_ROOT_PORT, UHCI_SYNTHETIC_HID_MOUSE_HUB_PORT];
const SYNTHETIC_USB_HID_GAMEPAD_PATH: GuestUsbPath = [EXTERNAL_HUB_ROOT_PORT, UHCI_SYNTHETIC_HID_GAMEPAD_HUB_PORT];
const MAX_SYNTHETIC_USB_HID_REPORTS_PER_INPUT_BATCH = 64;
const MAX_SYNTHETIC_USB_HID_OUTPUT_REPORTS_PER_TICK = 64;

let snapshotPaused = false;
let snapshotOpInFlight = false;

// Device blobs recovered from the most recent VM snapshot restore. We keep these around so that
// unknown/unhandled device state can roundtrip through restore → save without being silently
// dropped (forward compatibility).
//
// NOTE: These are stored as `Uint8Array` so the IO worker can keep a stable copy even when we
// transfer `ArrayBuffer` payloads to the coordinator.
let snapshotRestoredDeviceBlobs: Array<{ kind: string; bytes: Uint8Array }> = [];

// Many IO worker devices (notably audio DMA engines) advance guest-visible state based on
// host time deltas (`nowMs` passed to `DeviceManager.tick`). During VM snapshot
// save/restore we intentionally pause device ticking, but the browser's wall-clock
// continues to advance. If we resume with the raw `performance.now()` timestamp,
// devices can observe a huge delta and "fast-forward" (e.g. burst audio output or
// DMA position jumps).
//
// To keep snapshot pause/resume semantics deterministic, we maintain a monotonic
// "VM tick time" that does *not* advance while `snapshotPaused` is true. All
// devices are ticked against this virtual time.
const ioTickTimebase = new VmTimebase();
// While the IO worker is snapshot-paused we must not allow any asynchronous messages (e.g. WebUSB
// completions, WebHID input reports) to call into WASM and mutate guest RAM/device state; otherwise
// snapshot save would race with those writes and become nondeterministic.
//
// Queue the affected messages and replay them after resume.
const MAX_QUEUED_SNAPSHOT_PAUSED_MESSAGES = 4096;
const MAX_QUEUED_SNAPSHOT_PAUSED_BYTES = 16 * 1024 * 1024;
let queuedSnapshotPausedBytes = 0;
const queuedSnapshotPausedMessages: unknown[] = [];

const MAX_QUEUED_INPUT_BATCH_BYTES = 4 * 1024 * 1024;
let queuedInputBatchBytes = 0;
const queuedInputBatches: Array<{ buffer: ArrayBuffer; recycle: boolean }> = [];

function estimateQueuedSnapshotPausedBytes(msg: unknown): number {
  // We only estimate byte sizes for the high-frequency, byte-bearing message types.
  if (isUsbCompletionMessage(msg)) {
    const completion = msg.completion;
    if (completion.status === "success" && "data" in completion) {
      return completion.data.byteLength >>> 0;
    }
    return 0;
  }
  if (isHidInputReportMessage(msg)) {
    return msg.data.byteLength >>> 0;
  }
  if (isHidPassthroughInputReportMessage(msg)) {
    return msg.data.byteLength >>> 0;
  }
  return 0;
}

function queueSnapshotPausedMessage(msg: unknown): void {
  if (queuedSnapshotPausedMessages.length >= MAX_QUEUED_SNAPSHOT_PAUSED_MESSAGES) return;
  const estimated = estimateQueuedSnapshotPausedBytes(msg);
  if (queuedSnapshotPausedBytes + estimated > MAX_QUEUED_SNAPSHOT_PAUSED_BYTES) return;
  queuedSnapshotPausedMessages.push(msg);
  queuedSnapshotPausedBytes += estimated;
}

function flushQueuedSnapshotPausedMessages(): void {
  if (queuedSnapshotPausedMessages.length === 0) return;
  const queued = queuedSnapshotPausedMessages.splice(0, queuedSnapshotPausedMessages.length);
  queuedSnapshotPausedBytes = 0;
  for (const msg of queued) {
    // Replay the message to any listeners that were blocked during the snapshot pause.
    ctx.dispatchEvent(new MessageEvent("message", { data: msg }));
  }
}

function flushQueuedInputBatches(): void {
  if (queuedInputBatches.length === 0) return;
  const batches = queuedInputBatches.splice(0, queuedInputBatches.length);
  queuedInputBatchBytes = 0;
  for (const entry of batches) {
    if (started) {
      handleInputBatch(entry.buffer);
    }
    if (entry.recycle) {
      ctx.postMessage({ type: "in:input-batch-recycle", buffer: entry.buffer } satisfies InputBatchRecycleMessage, [entry.buffer]);
    }
  }
}

function copyU8ToArrayBuffer(bytes: Uint8Array): ArrayBuffer {
  const out = new Uint8Array(bytes.byteLength);
  out.set(bytes);
  return out.buffer;
}

// Intercept async input-related messages while snapshot-paused so WebUSB/WebHID runtimes (and other
// listeners) cannot apply them and mutate guest RAM/device state mid-snapshot.
//
// Use a capturing listener so we run before any runtime-added `addEventListener("message", ...)`
// handlers (which would otherwise process the completion immediately).
ctx.addEventListener(
  "message",
  (ev) => {
    if (!snapshotPaused) return;
    const data = (ev as MessageEvent<unknown>).data;
    // Input batches are queued separately so buffers can be recycled after processing.
    if ((data as Partial<InputBatchMessage>)?.type === "in:input-batch") return;

    const shouldQueue =
      isUsbCompletionMessage(data) ||
      isUsbSelectedMessage(data) ||
      isUsbRingAttachMessage(data) ||
      isUsbRingDetachMessage(data) ||
      isHidProxyMessage(data) ||
      isHidPassthroughAttachHubMessage(data) ||
      isHidPassthroughAttachMessage(data) ||
      isHidPassthroughDetachMessage(data) ||
      isHidPassthroughInputReportMessage(data);
    if (!shouldQueue) return;
    ev.stopImmediatePropagation();
    queueSnapshotPausedMessage(data);
  },
  { capture: true },
);

function snapshotUsbDeviceState(): { kind: string; bytes: Uint8Array } | null {
  const runtime = uhciRuntime;
  if (runtime) {
    const save =
      (runtime as unknown as { save_state?: unknown }).save_state ?? (runtime as unknown as { snapshot_state?: unknown }).snapshot_state;
    if (typeof save === "function") {
      try {
        const bytes = save.call(runtime) as unknown;
        if (bytes instanceof Uint8Array) return { kind: VM_SNAPSHOT_DEVICE_USB_KIND, bytes };
      } catch (err) {
        console.warn("[io.worker] UhciRuntime save_state failed:", err);
      }
    }
  }

  const bridge = uhciControllerBridge;
  if (bridge) {
    const save =
      (bridge as unknown as { save_state?: unknown }).save_state ?? (bridge as unknown as { snapshot_state?: unknown }).snapshot_state;
    if (typeof save === "function") {
      try {
        const bytes = save.call(bridge) as unknown;
        if (bytes instanceof Uint8Array) return { kind: VM_SNAPSHOT_DEVICE_USB_KIND, bytes };
      } catch (err) {
        console.warn("[io.worker] UhciControllerBridge save_state failed:", err);
      }
    }
  }

  return null;
}

function snapshotI8042DeviceState(): { kind: string; bytes: Uint8Array } | null {
  if (i8042Wasm) {
    try {
      const bytes = i8042Wasm.save_state();
      if (bytes instanceof Uint8Array) return { kind: VM_SNAPSHOT_DEVICE_I8042_KIND, bytes };
    } catch (err) {
      console.warn("[io.worker] I8042Bridge save_state failed:", err);
    }
  }
  if (i8042Ts) {
    try {
      return { kind: VM_SNAPSHOT_DEVICE_I8042_KIND, bytes: i8042Ts.saveState() };
    } catch (err) {
      console.warn("[io.worker] i8042 saveState failed:", err);
    }
  }
  return null;
}

function snapshotE1000DeviceState(): { kind: string; bytes: Uint8Array } | null {
  const bridge = e1000Bridge;
  if (!bridge) return null;

  const save =
    (bridge as unknown as { save_state?: unknown }).save_state ??
    (bridge as unknown as { snapshot_state?: unknown }).snapshot_state;
  if (typeof save !== "function") return null;
  try {
    const bytes = save.call(bridge) as unknown;
    if (bytes instanceof Uint8Array) return { kind: VM_SNAPSHOT_DEVICE_E1000_KIND, bytes };
  } catch (err) {
    console.warn("[io.worker] E1000 save_state failed:", err);
  }
  return null;
}

function restoreUsbDeviceState(bytes: Uint8Array): void {
  const runtime = uhciRuntime;
  if (runtime) {
    const load =
      (runtime as unknown as { load_state?: unknown }).load_state ?? (runtime as unknown as { restore_state?: unknown }).restore_state;
    if (typeof load === "function") {
      load.call(runtime, bytes);
      return;
    }
  }

  const bridge = uhciControllerBridge;
  if (bridge) {
    const load =
      (bridge as unknown as { load_state?: unknown }).load_state ?? (bridge as unknown as { restore_state?: unknown }).restore_state;
    if (typeof load === "function") {
      load.call(bridge, bytes);
      return;
    }
  }
}

function restoreI8042DeviceState(bytes: Uint8Array): void {
  if (i8042Wasm) {
    try {
      i8042Wasm.load_state(bytes);
    } catch (err) {
      console.warn("[io.worker] I8042Bridge load_state failed:", err);
    }
    return;
  }
  if (i8042Ts) {
    try {
      i8042Ts.loadState(bytes);
    } catch (err) {
      console.warn("[io.worker] i8042 loadState failed:", err);
    }
  }
}

function restoreE1000DeviceState(bytes: Uint8Array): void {
  // The E1000 NIC is optional and may not be initialized if virtio-net is present. If the snapshot
  // includes E1000 state but virtio-net is absent, attempt to initialize the NIC before applying
  // state so snapshots remain forwards-compatible across runtime builds.
  if (!e1000Bridge && !virtioNetDevice) {
    maybeInitE1000Device();
  }

  const bridge = e1000Bridge;
  if (!bridge) return;

  const load =
    (bridge as unknown as { load_state?: unknown }).load_state ??
    (bridge as unknown as { restore_state?: unknown }).restore_state;
  if (typeof load !== "function") return;

  try {
    load.call(bridge, bytes);
  } catch (err) {
    console.warn("[io.worker] E1000 load_state failed:", err);
  }

  try {
    e1000Device?.onSnapshotRestore();
  } catch {
    // ignore
  }
}

type AudioHdaSnapshotBridgeLike = {
  save_state?: () => Uint8Array;
  snapshot_state?: () => Uint8Array;
  load_state?: (bytes: Uint8Array) => void;
  restore_state?: (bytes: Uint8Array) => void;
};

// Optional HDA audio device bridge. This is populated by the audio integration when present.
let audioHdaBridge: AudioHdaSnapshotBridgeLike | null = null;

function resolveAudioHdaSnapshotBridge(): AudioHdaSnapshotBridgeLike | null {
  if (audioHdaBridge) return audioHdaBridge;

  // Allow other runtimes/experiments to attach an HDA bridge via a well-known global.
  // This is intentionally best-effort so snapshots can still be loaded in environments
  // without audio support.
  const anyGlobal = globalThis as unknown as Record<string, unknown>;
  const candidate =
    anyGlobal["__aeroAudioHdaBridge"] ??
    anyGlobal["__aero_hda_bridge"] ??
    anyGlobal["__aero_audio_hda_bridge"] ??
    anyGlobal["__aero_io_hda_bridge"] ??
    null;
  if (!candidate) return null;
  if (typeof candidate !== "object" && typeof candidate !== "function") return null;
  return candidate as AudioHdaSnapshotBridgeLike;
}

function snapshotAudioHdaDeviceState(): { kind: string; bytes: Uint8Array } | null {
  const bridge = resolveAudioHdaSnapshotBridge();
  if (!bridge) return null;

  const save =
    (bridge as unknown as { save_state?: unknown }).save_state ?? (bridge as unknown as { snapshot_state?: unknown }).snapshot_state;
  if (typeof save !== "function") return null;
  try {
    const bytes = save.call(bridge) as unknown;
    if (bytes instanceof Uint8Array) return { kind: VM_SNAPSHOT_DEVICE_AUDIO_HDA_KIND, bytes };
  } catch (err) {
    console.warn("[io.worker] HDA audio save_state failed:", err);
  }
  return null;
}

function restoreAudioHdaDeviceState(bytes: Uint8Array): void {
  const bridge = resolveAudioHdaSnapshotBridge();
  if (!bridge) return;

  const load =
    (bridge as unknown as { load_state?: unknown }).load_state ?? (bridge as unknown as { restore_state?: unknown }).restore_state;
  if (typeof load !== "function") return;
  load.call(bridge, bytes);
}

function snapshotE1000DeviceState(): { kind: string; bytes: Uint8Array } | null {
  const bridge = e1000Bridge;
  if (!bridge) return null;

  const save =
    (bridge as unknown as { save_state?: unknown }).save_state ?? (bridge as unknown as { snapshot_state?: unknown }).snapshot_state;
  if (typeof save !== "function") return null;
  try {
    const bytes = save.call(bridge) as unknown;
    if (bytes instanceof Uint8Array) return { kind: VM_SNAPSHOT_DEVICE_E1000_KIND, bytes };
  } catch (err) {
    console.warn("[io.worker] E1000 save_state failed:", err);
  }
  return null;
}

function restoreE1000DeviceState(bytes: Uint8Array): void {
  const bridge = e1000Bridge;
  if (!bridge) return;

  const load =
    (bridge as unknown as { load_state?: unknown }).load_state ?? (bridge as unknown as { restore_state?: unknown }).restore_state;
  if (typeof load !== "function") return;
  try {
    load.call(bridge, bytes);
  } catch (err) {
    console.warn("[io.worker] E1000 load_state failed:", err);
  }
}

// Keep broker IDs from overlapping between multiple concurrent USB action sources (UHCI runtime,
// harness panel, demo driver, etc). The demo uses 1_000_000_000 and the harness uses 2_000_000_000.
const UHCI_RUNTIME_WEBUSB_ID_BASE = 3_000_000_000;

function rewriteUsbHostActionId(action: UsbHostAction, id: number): UsbHostAction {
  switch (action.kind) {
    case "controlIn":
      return { kind: "controlIn", id, setup: action.setup };
    case "controlOut":
      return { kind: "controlOut", id, setup: action.setup, data: action.data };
    case "bulkIn":
      return { kind: "bulkIn", id, endpoint: action.endpoint, length: action.length };
    case "bulkOut":
      return { kind: "bulkOut", id, endpoint: action.endpoint, data: action.data };
    default: {
      const neverKind: never = action;
      throw new Error(`Unknown UsbHostAction kind: ${String((neverKind as unknown as { kind?: unknown }).kind)}`);
    }
  }
}

function rewriteUsbHostCompletionId(completion: UsbHostCompletion, id: number): UsbHostCompletion {
  switch (completion.kind) {
    case "controlIn":
    case "bulkIn":
      if (completion.status === "success") return { kind: completion.kind, id, status: "success", data: completion.data };
      if (completion.status === "stall") return { kind: completion.kind, id, status: "stall" };
      return { kind: completion.kind, id, status: "error", message: completion.message };
    case "controlOut":
    case "bulkOut":
      if (completion.status === "success") {
        return { kind: completion.kind, id, status: "success", bytesWritten: completion.bytesWritten };
      }
      if (completion.status === "stall") return { kind: completion.kind, id, status: "stall" };
      return { kind: completion.kind, id, status: "error", message: completion.message };
    default: {
      const neverKind: never = completion;
      throw new Error(`Unknown UsbHostCompletion kind: ${String((neverKind as unknown as { kind?: unknown }).kind)}`);
    }
  }
}

class UhciRuntimeWebUsbBridge {
  readonly #uhci: UhciRuntimeInstance;
  readonly #rootPort: number;
  readonly #onStateChange?: () => void;

  #connected = false;
  #desiredConnected: boolean | null = null;
  #applyScheduled = false;
  #resetScheduled = false;
  #lastError: string | null = null;
  #nextBrokerId = UHCI_RUNTIME_WEBUSB_ID_BASE;
  readonly #pendingByBrokerId = new Map<number, { wasmId: number; kind: UsbHostAction["kind"] }>();

  constructor(opts: { uhci: UhciRuntimeInstance; rootPort: number; onStateChange?: () => void }) {
    this.#uhci = opts.uhci;
    this.#rootPort = opts.rootPort >>> 0;
    this.#onStateChange = opts.onStateChange;
  }

  set_connected(connected: boolean): void {
    this.#desiredConnected = Boolean(connected);
    this.#scheduleApply();
  }

  is_connected(): boolean {
    return this.#connected;
  }

  last_error(): string | null {
    return this.#lastError;
  }

  drain_actions(): UsbHostAction[] | null {
    // If hotplug is requested, attempt to apply it opportunistically during the normal polling
    // path. This avoids relying solely on timers/microtasks to run the attach/detach operation.
    this.#applyDesired();
    if (!this.#connected) return null;
    const actions = this.#uhci.webusb_drain_actions();
    if (!Array.isArray(actions) || actions.length === 0) return null;

    const out: UsbHostAction[] = [];
    for (const action of actions) {
      const brokerId = this.allocBrokerId();
      this.#pendingByBrokerId.set(brokerId, { wasmId: action.id, kind: action.kind });
      out.push(rewriteUsbHostActionId(action, brokerId));
    }
    return out.length === 0 ? null : out;
  }

  push_completion(completion: UsbHostCompletion): void {
    const mapping = this.#pendingByBrokerId.get(completion.id);
    if (!mapping) return;
    this.#pendingByBrokerId.delete(completion.id);

    let rewritten: UsbHostCompletion;
    if (completion.kind !== mapping.kind) {
      rewritten = {
        kind: mapping.kind,
        id: mapping.wasmId,
        status: "error",
        message: `USB completion kind mismatch (expected ${mapping.kind}, got ${completion.kind})`,
      };
    } else {
      rewritten = rewriteUsbHostCompletionId(completion, mapping.wasmId);
    }

    this.#uhci.webusb_push_completion(rewritten);
  }

  reset(): void {
    this.#pendingByBrokerId.clear();
    if (!this.#connected) return;
    // If we're in the process of disconnecting, do not reattach.
    if (this.#desiredConnected === false) {
      this.#scheduleApply();
      return;
    }

    // `UsbWebUsbPassthroughDevice` doesn't currently expose a "soft reset" hook.
    // Detach+reattach clears in-flight control transfers so the guest isn't stuck NAKing forever.
    try {
      this.#uhci.webusb_detach();
      this.#connected = false;
    } catch (err) {
      if (this.#isRecursiveBorrowError(err)) {
        this.#scheduleReset();
        return;
      }
      this.#lastError = err instanceof Error ? err.message : String(err);
      this.#onStateChange?.();
      return;
    }

    try {
      const assigned = this.#uhci.webusb_attach(this.#rootPort);
      this.#connected = true;
      this.#lastError = null;
      if (assigned !== this.#rootPort) {
        console.warn(`[io.worker] UhciRuntime.webusb_attach assigned unexpected port=${assigned} (wanted ${this.#rootPort})`);
      }
      this.#onStateChange?.();
    } catch (err) {
      if (this.#isRecursiveBorrowError(err)) {
        // We are detached at this point. Request reconnection and let the normal apply/retry
        // loop handle it on the next turn.
        this.#desiredConnected = true;
        this.#scheduleApply();
        return;
      }
      this.#lastError = err instanceof Error ? err.message : String(err);
      this.#onStateChange?.();
    }
  }

  pending_summary(): unknown {
    return {
      connected: this.#connected,
      pending: this.#pendingByBrokerId.size,
      nextBrokerId: this.#nextBrokerId,
    };
  }

  free(): void {
    // The UHCI runtime itself is owned by the UHCI PCI device bridge.
    this.#pendingByBrokerId.clear();
  }

  private allocBrokerId(): number {
    const id = this.#nextBrokerId;
    this.#nextBrokerId += 1;
    if (!Number.isSafeInteger(id) || id < 0 || id > 0xffff_ffff) {
      throw new Error(`UhciRuntimeWebUsbBridge ran out of valid broker action IDs (next=${this.#nextBrokerId})`);
    }
    return id;
  }

  #isRecursiveBorrowError(err: unknown): boolean {
    const message = err instanceof Error ? err.message : String(err);
    return message.includes("recursive use of an object detected") || message.includes("UHCI runtime is busy");
  }

  #scheduleApply(): void {
    if (this.#applyScheduled) return;
    this.#applyScheduled = true;
    setTimeout(() => {
      this.#applyScheduled = false;
      this.#applyDesired();
    }, 0);
  }

  #scheduleReset(): void {
    if (this.#resetScheduled) return;
    this.#resetScheduled = true;
    setTimeout(() => {
      this.#resetScheduled = false;
      this.reset();
    }, 0);
  }

  #applyDesired(): void {
    const desired = this.#desiredConnected;
    if (desired === null || desired === this.#connected) return;

    try {
      if (!desired) {
        this.#uhci.webusb_detach();
        this.#connected = false;
        this.#desiredConnected = null;
        this.#lastError = null;
        this.#onStateChange?.();
        return;
      }

      const assigned = this.#uhci.webusb_attach(this.#rootPort);
      this.#connected = true;
      this.#desiredConnected = null;
      this.#lastError = null;

      if (assigned !== this.#rootPort) {
        console.warn(`[io.worker] UhciRuntime.webusb_attach assigned unexpected port=${assigned} (wanted ${this.#rootPort})`);
      }
      this.#onStateChange?.();
    } catch (err) {
      if (this.#isRecursiveBorrowError(err)) {
        // wasm-bindgen guards &mut self methods against reentrancy by throwing
        // "recursive use of an object..." when the object is already borrowed.
        // This can happen when WebUSB hotplug races other UHCI calls (ticks, HID polling).
        // Retry on the next turn rather than failing the guest attachment permanently.
        //
        // Surface a more user-friendly status message so callers (and tests) can distinguish
        // "still retrying" from "never attempted".
        if (this.#lastError === null) {
          const message = err instanceof Error ? err.message : String(err);
          this.#lastError = `UHCI runtime is busy; retrying WebUSB hotplug. (${message})`;
          this.#onStateChange?.();
        }
        setTimeout(() => this.#scheduleApply(), 0);
        return;
      }
      this.#lastError = err instanceof Error ? err.message : String(err);
      this.#onStateChange?.();
    }
  }
}

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

function maybeSendWasmReady(): void {
  if (wasmReadySent) return;
  const init = pendingWasmInit;
  if (!init) return;
  // Readiness implies the guest-visible UHCI controller is registered and the WASM-side HID
  // passthrough bridge is installed, so `hid.attach` can immediately hotplug into the guest.
  if (!uhciDevice) return;
  if (!wasmHidGuest) return;
  try {
    if (Atomics.load(status, StatusIndex.StopRequested) === 1) return;
  } catch {
    // ignore if status isn't initialized yet.
  }

  let value = 0;
  try {
    value = init.api.add(20, 22);
  } catch {
    value = 0;
  }
  ctx.postMessage({ type: MessageType.WASM_READY, role, variant: init.variant, value } satisfies ProtocolMessage);
  wasmReadySent = true;
}

function maybeInitUhciRuntime(): void {
  if (uhciRuntime || uhciDevice) return;
  const api = wasmApi;
  const mgr = deviceManager;
  if (!api || !mgr) return;
  const Ctor = api.UhciRuntime;
  if (!Ctor) return;
  if (!guestBase || !guestSize) return;

  let runtime: UhciRuntimeInstance;
  try {
    runtime = new Ctor(guestBase >>> 0, guestSize >>> 0);
  } catch (err) {
    console.warn("[io.worker] Failed to initialize UHCI runtime", err);
    return;
  }

  const bridge: UhciControllerBridgeLike = {
    io_read: (offset, size) => runtime.port_read(offset >>> 0, size >>> 0) >>> 0,
    io_write: (offset, size, value) => runtime.port_write(offset >>> 0, size >>> 0, value >>> 0),
    step_frame: () => runtime.step_frame(),
    tick_1ms: () => runtime.tick_1ms(),
    irq_asserted: () => runtime.irq_level(),
    free: () => {
      try {
        runtime.free();
      } catch {
        // ignore
      }
    },
  };

  try {
    const dev = new UhciPciDevice({ bridge, irqSink: mgr.irqSink });
    uhciDevice = dev;
    uhciRuntime = runtime;
    uhciRuntimeHubConfig.apply(runtime, {
      warn: (message, err) => console.warn(`[io.worker] ${message}`, err),
    });
    mgr.registerPciDevice(dev);
    mgr.addTickable(dev);
    uhciHidTopology.setUhciBridge(null);
  } catch (err) {
    console.warn("[io.worker] Failed to register UHCI runtime PCI device", err);
    try {
      runtime.free();
    } catch {
      // ignore
    }
    uhciRuntime = null;
    uhciDevice = null;
  }
}

function maybeInitE1000Device(): void {
  if (e1000Device) return;
  // Only one NIC can be attached to the shared NET_TX/NET_RX rings at a time.
  // If virtio-net is available, prefer it and keep E1000 as a fallback.
  if (virtioNetDevice) return;
  const api = wasmApi;
  const mgr = deviceManager;
  if (!api || !mgr) return;
  const Bridge = api.E1000Bridge;
  if (!Bridge) return;
  if (!guestBase) return;
  const txRing = netTxRing;
  const rxRing = netRxRing;
  if (!txRing || !rxRing) return;

  let bridge: E1000Bridge;
  try {
    // wasm-bindgen's JS glue may enforce exact constructor arity. Prefer the 3-argument
    // form (guestBase, guestSize, mac?) but fall back to the 2-argument form if needed.
    //
    // `guestSize=0` is treated as "use remainder of linear memory" by the Rust bridge.
    const base = guestBase >>> 0;
    const size = guestSize >>> 0;
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const Ctor = Bridge as any;
    try {
      bridge = Ctor.length >= 3 ? new Ctor(base, size, undefined) : new Ctor(base, size);
    } catch {
      // Retry with opposite arity (supports older/newer wasm-bindgen outputs).
      bridge = Ctor.length >= 3 ? new Ctor(base, size) : new Ctor(base, size, undefined);
    }
  } catch (err) {
    console.warn("[io.worker] Failed to initialize E1000 bridge", err);
    return;
  }

  try {
    const dev = new E1000PciDevice({ bridge, irqSink: mgr.irqSink, netTxRing: txRing, netRxRing: rxRing });
    e1000Bridge = bridge;
    e1000Device = dev;
    mgr.registerPciDevice(dev);
    mgr.addTickable(dev);
  } catch (err) {
    console.warn("[io.worker] Failed to register E1000 PCI device", err);
    try {
      bridge.free();
    } catch {
      // ignore
    }
    e1000Bridge = null;
    e1000Device = null;
  }
}

function maybeInitVirtioInput(): void {
  if (virtioInputKeyboard || virtioInputMouse) return;
  const api = wasmApi;
  const mgr = deviceManager;
  if (!api || !mgr) return;
  const Ctor = api.VirtioInputPciDevice;
  if (!Ctor) return;
  if (!guestBase) return;

  const base = guestBase >>> 0;
  const size = guestSize >>> 0;

  // wasm-bindgen's JS glue can enforce constructor arity; try a few common layouts.
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const AnyCtor = Ctor as any;
  let keyboardDev: VirtioInputPciDevice | null = null;
  let mouseDev: VirtioInputPciDevice | null = null;
  try {
    try {
      keyboardDev = new AnyCtor(base, size, "keyboard");
    } catch {
      keyboardDev = new AnyCtor("keyboard", base, size);
    }

    try {
      mouseDev = new AnyCtor(base, size, "mouse");
    } catch {
      mouseDev = new AnyCtor("mouse", base, size);
    }
  } catch (err) {
    console.warn("[io.worker] Failed to initialize virtio-input devices", err);
    try {
      (keyboardDev as unknown as { free?: () => void } | null)?.free?.();
    } catch {
      // ignore
    }
    try {
      (mouseDev as unknown as { free?: () => void } | null)?.free?.();
    } catch {
      // ignore
    }
    return;
  }

  let keyboardFn: VirtioInputPciFunction | null = null;
  let mouseFn: VirtioInputPciFunction | null = null;
  let keyboardRegistered = false;
  try {
    keyboardFn = new VirtioInputPciFunction({ kind: "keyboard", device: keyboardDev as unknown as any, irqSink: mgr.irqSink });
    mouseFn = new VirtioInputPciFunction({ kind: "mouse", device: mouseDev as unknown as any, irqSink: mgr.irqSink });

    // Register as a single multi-function PCI device:
    // - function 0: keyboard
    // - function 1: mouse
    //
    // Prefer the canonical BDF used by the Rust "canonical IO bus" (0:10.0 / 0:10.1). If that
    // slot is already occupied, fall back to auto allocation while still keeping both functions
    // on the same PCI device number.
    const { addr: keyboardAddr } = registerVirtioInputKeyboardPciFunction({ mgr, keyboardFn });
    keyboardRegistered = true;
    mgr.addTickable(keyboardFn);
    const mouseAddr = mgr.registerPciDevice(mouseFn, { device: keyboardAddr.device, function: 1 });
    // Keep bdf consistent with actual assigned addresses (useful for debugging).
    (mouseFn as unknown as { bdf?: typeof mouseAddr }).bdf = mouseAddr;
    mgr.addTickable(mouseFn);

    virtioInputKeyboard = keyboardFn;
    virtioInputMouse = mouseFn;
  } catch (err) {
    console.warn("[io.worker] Failed to register virtio-input PCI functions", err);
    if (!keyboardRegistered) {
      try {
        keyboardFn?.destroy();
      } catch {
        // ignore
      }
    } else {
      // Function 0 is already registered on the PCI bus; keep the wrapper alive.
      virtioInputKeyboard = keyboardFn;
    }
    try {
      mouseFn?.destroy();
    } catch {
      // ignore
    }
    // If wrapper construction failed before we took ownership, free the raw WASM objects.
    if (!keyboardFn) {
      try {
        (keyboardDev as unknown as { free?: () => void } | null)?.free?.();
      } catch {
        // ignore
      }
    }
    if (!mouseFn) {
      try {
        (mouseDev as unknown as { free?: () => void } | null)?.free?.();
      } catch {
        // ignore
      }
    }
    if (!keyboardRegistered) virtioInputKeyboard = null;
    virtioInputMouse = null;
  }
}

function maybeInitUhciDevice(): void {
  const api = wasmApi;
  const mgr = deviceManager;
  if (!api || !mgr) return;
  if (!guestBase) return;

  if (api.UhciRuntime) {
    maybeInitUhciRuntime();
  }

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

  // Synthetic USB HID devices (keyboard/mouse/gamepad) are attached behind the external hub once
  // a guest-visible UHCI controller exists.
  maybeInitSyntheticUsbHidDevices();

  if (!webUsbGuestBridge) {
    if (uhciRuntime) {
      let runtimeBridge: UhciRuntimeWebUsbBridge;
      runtimeBridge = new UhciRuntimeWebUsbBridge({
        uhci: uhciRuntime,
        rootPort: WEBUSB_GUEST_ROOT_PORT,
        onStateChange: () => {
          webUsbGuestAttached = runtimeBridge.is_connected();
          webUsbGuestLastError = runtimeBridge.last_error();
          emitWebUsbGuestStatus();
        },
      });
      webUsbGuestBridge = runtimeBridge;
      uhciRuntimeWebUsbBridge = runtimeBridge;

      if (!usbPassthroughRuntime) {
        usbPassthroughRuntime = new WebUsbPassthroughRuntime({
          bridge: runtimeBridge,
          port: ctx,
          pollIntervalMs: 0,
          initiallyBlocked: !usbAvailable,
          initialRingAttach: usbRingAttach ?? undefined,
         });
         usbPassthroughRuntime.start();
         if (import.meta.env.DEV) {
           const timer = setInterval(() => {
             console.debug("[io.worker] UHCI runtime WebUSB pending_summary()", usbPassthroughRuntime?.pendingSummary());
           }, 1000) as unknown as number;
           (timer as unknown as { unref?: () => void }).unref?.();
           usbPassthroughDebugTimer = timer;
         }
       }

      let applyError: string | null = null;
      if (lastUsbSelected) {
        try {
          applyUsbSelectedToWebUsbUhciBridge(runtimeBridge, lastUsbSelected);
        } catch (err) {
          console.warn("[io.worker] Failed to apply usb.selected to UHCI runtime WebUSB bridge", err);
          applyError = `Failed to apply usb.selected to UHCI runtime: ${formatWebUsbGuestError(err)}`;
        }
      }

      webUsbGuestAttached = runtimeBridge.is_connected();
      webUsbGuestLastError = applyError ?? runtimeBridge.last_error();
      emitWebUsbGuestStatus();
      return;
    }

    const bridge = uhciControllerBridge;
    const hasWebUsb =
      bridge &&
      typeof (bridge as unknown as { set_connected?: unknown }).set_connected === "function" &&
      typeof (bridge as unknown as { drain_actions?: unknown }).drain_actions === "function" &&
      typeof (bridge as unknown as { push_completion?: unknown }).push_completion === "function" &&
      typeof (bridge as unknown as { reset?: unknown }).reset === "function";

    if (bridge && hasWebUsb) {
      // `UhciPciDevice` owns the WASM bridge and calls `free()` during shutdown; wrap with a
      // no-op `free()` so `WebUsbPassthroughRuntime` does not double-free.
      const wrapped: WebUsbGuestBridge = {
        set_connected: (connected) => bridge.set_connected(connected),
        drain_actions: () => bridge.drain_actions(),
        push_completion: (completion) => bridge.push_completion(completion),
        reset: () => bridge.reset(),
        // Debug-only; tolerate older WASM builds that might not expose it.
        pending_summary: () => {
          const fn = (bridge as unknown as { pending_summary?: unknown }).pending_summary;
          if (typeof fn !== "function") return null;
          return fn.call(bridge) as unknown;
        },
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
         const timer = setInterval(() => {
           console.debug("[io.worker] UHCI WebUSB pending_summary()", usbPassthroughRuntime?.pendingSummary());
         }, 1000) as unknown as number;
         (timer as unknown as { unref?: () => void }).unref?.();
         usbPassthroughDebugTimer = timer;
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

function maybeInitHdaDevice(): void {
  if (hdaDevice) return;
  const api = wasmApi;
  const mgr = deviceManager;
  if (!api || !mgr) return;
  if (!guestBase || !guestSize) return;

  const Bridge = api.HdaControllerBridge;
  if (!Bridge) return;

  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const Ctor = Bridge as any;
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  let bridge: any;
  try {
    const base = guestBase >>> 0;
    const size = guestSize >>> 0;
    try {
      bridge = Ctor.length >= 2 ? new Ctor(base, size) : new Ctor(base);
    } catch {
      // Retry with the opposite arity to support older/newer wasm-bindgen outputs.
      bridge = Ctor.length >= 2 ? new Ctor(base) : new Ctor(base, size);
    }
    const dev = new HdaPciDevice({ bridge: bridge as HdaControllerBridgeLike, irqSink: mgr.irqSink });
    hdaControllerBridge = bridge;
    hdaDevice = dev;
    mgr.registerPciDevice(dev);
    mgr.addTickable(dev);

    // Apply any existing microphone ring-buffer attachment.
    if (micRingBuffer) {
      dev.setMicRingBuffer(micRingBuffer);
      if (micSampleRate > 0) dev.setCaptureSampleRateHz(micSampleRate);
    }

    // Apply any existing audio output ring-buffer attachment (producer-side).
    if (audioOutRingBuffer) {
      dev.setAudioRingBuffer({
        ringBuffer: audioOutRingBuffer,
        capacityFrames: audioOutCapacityFrames,
        channelCount: audioOutChannelCount,
        dstSampleRateHz: audioOutDstSampleRate,
      });
    }
  } catch (err) {
    console.warn("[io.worker] Failed to initialize HDA controller bridge", err);
    try {
      bridge?.free?.();
    } catch {
      // ignore
    }
    hdaControllerBridge = null;
    hdaDevice = null;
  }
}

function maybeInitSyntheticUsbHidDevices(): void {
  if (syntheticUsbHidAttached) return;
  const api = wasmApi;
  if (!api) return;
  const Bridge = api.UsbHidPassthroughBridge;
  if (!Bridge) return;

  // Ensure a UHCI controller is registered before attaching devices. This is required both for
  // the legacy `UhciControllerBridge` path and the newer `UhciRuntime` path.
  if (!uhciDevice) return;

  try {
    if (!syntheticUsbKeyboard) {
      syntheticUsbKeyboard = new Bridge(
        0x1234,
        0x0001,
        "Aero",
        "Aero USB Keyboard",
        undefined,
        USB_HID_BOOT_KEYBOARD_REPORT_DESCRIPTOR,
        false,
        USB_HID_INTERFACE_SUBCLASS_BOOT,
        USB_HID_INTERFACE_PROTOCOL_KEYBOARD,
      );
    }
    if (!syntheticUsbMouse) {
      syntheticUsbMouse = new Bridge(
        0x1234,
        0x0002,
        "Aero",
        "Aero USB Mouse",
        undefined,
        USB_HID_BOOT_MOUSE_REPORT_DESCRIPTOR,
        false,
        USB_HID_INTERFACE_SUBCLASS_BOOT,
        USB_HID_INTERFACE_PROTOCOL_MOUSE,
      );
    }
    if (!syntheticUsbGamepad) {
      syntheticUsbGamepad = new Bridge(
        0x1234,
        0x0003,
        "Aero",
        "Aero USB Gamepad",
        undefined,
        USB_HID_GAMEPAD_REPORT_DESCRIPTOR,
        false,
        undefined,
        undefined,
      );
    }
  } catch (err) {
    console.warn("[io.worker] Failed to construct synthetic USB HID devices", err);
    return;
  }

  // UHCI runtime path: attach via a runtime-exported helper, if available.
  const runtime = uhciRuntime as unknown as { attach_usb_hid_passthrough_device?: unknown } | null;
  if (runtime && typeof runtime.attach_usb_hid_passthrough_device === "function") {
    try {
      runtime.attach_usb_hid_passthrough_device.call(runtime, SYNTHETIC_USB_HID_KEYBOARD_PATH, syntheticUsbKeyboard);
      runtime.attach_usb_hid_passthrough_device.call(runtime, SYNTHETIC_USB_HID_MOUSE_PATH, syntheticUsbMouse);
      runtime.attach_usb_hid_passthrough_device.call(runtime, SYNTHETIC_USB_HID_GAMEPAD_PATH, syntheticUsbGamepad);
      syntheticUsbHidAttached = true;
    } catch (err) {
      console.warn("[io.worker] Failed to attach synthetic USB HID devices to UHCI runtime", err);
    }
    return;
  }

  // Legacy controller bridge path: use the topology manager so hub attachments + reattachments are handled consistently.
  if (uhciControllerBridge) {
    uhciHidTopology.attachDevice(
      SYNTHETIC_USB_HID_KEYBOARD_DEVICE_ID,
      SYNTHETIC_USB_HID_KEYBOARD_PATH,
      "usb-hid-passthrough",
      syntheticUsbKeyboard,
    );
    uhciHidTopology.attachDevice(
      SYNTHETIC_USB_HID_MOUSE_DEVICE_ID,
      SYNTHETIC_USB_HID_MOUSE_PATH,
      "usb-hid-passthrough",
      syntheticUsbMouse,
    );
    uhciHidTopology.attachDevice(
      SYNTHETIC_USB_HID_GAMEPAD_DEVICE_ID,
      SYNTHETIC_USB_HID_GAMEPAD_PATH,
      "usb-hid-passthrough",
      syntheticUsbGamepad,
    );
    syntheticUsbHidAttached = true;
  }
}

function maybeInitVirtioNetDevice(): void {
  // Only one NIC can be attached to the shared NET_TX/NET_RX rings at a time.
  // If we already registered the E1000 fallback, don't attempt to add virtio-net.
  if (e1000Device) return;
  if (virtioNetDevice) return;
  const dev = tryInitVirtioNetDevice({
    api: wasmApi,
    mgr: deviceManager,
    guestBase,
    guestSize,
    ioIpc: ioIpcSab,
  });
  if (dev) {
    virtioNetDevice = dev;
  }
}

type UsbPassthroughDemo = InstanceType<NonNullable<WasmApi["UsbPassthroughDemo"]>>;
let usbDemo: UsbPassthroughDemoRuntime | null = null;
let usbDemoApi: UsbPassthroughDemo | null = null;
let usbDemoLastReportedError: string | null = null;

function resetUsbDemoErrorDedup(): void {
  usbDemoLastReportedError = null;
}

function emitUsbDemoError(message: string): void {
  if (message === usbDemoLastReportedError) return;
  usbDemoLastReportedError = message;
  ctx.postMessage({
    type: "usb.demoResult",
    result: { status: "error", message },
  } satisfies UsbPassthroughDemoResultMessage);
}

function handleUsbDemoFailure(context: string, err: unknown): void {
  console.warn(`[io.worker] UsbPassthroughDemo ${context} failed`, err);
  emitUsbDemoError(`UsbPassthroughDemo ${context} failed: ${formatWebUsbGuestError(err)}`);
  try {
    usbDemo?.reset();
  } catch {
    // ignore
  }
}

let hidInputRing: HidReportRing | null = null;
let hidOutputRing: HidReportRing | null = null;

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
  uhciRuntimeHubConfig.setPending(msg.guestPath, msg.portCount);
  uhciRuntimeHubConfig.apply(uhciRuntime, {
    warn: (message, err) => console.warn(`[io.worker] ${message}`, err),
  });
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
      `[hid] attach deviceId=${msg.deviceId} path=${guestPath.join(".")} vid=${hex16(msg.vendorId)} pid=${hex16(msg.productId)}`,
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
  const api = wasmApi;
  if (!api) return;

  // Ensure guest-visible USB controllers are registered before wiring up WebHID devices. If we
  // initialize the bridge before the UHCI controller exists, devices would never be visible to the
  // guest OS (PCI hotplug isn't modeled yet).
  maybeInitUhciDevice();
  if (wasmHidGuest) {
    maybeSendWasmReady();
    return;
  }
  if (!uhciRuntime && api.UhciControllerBridge && !uhciControllerBridge) return;

  try {
    if (uhciRuntime) {
      uhciRuntimeHidGuest = new WasmUhciHidGuestBridge({ uhci: uhciRuntime, host: hidHostSink });
      wasmHidGuest = uhciRuntimeHidGuest;
    } else {
      if (api.UhciControllerBridge && !uhciControllerBridge) return;
      wasmHidGuest = new WasmHidGuestBridge(api, hidHostSink, uhciHidTopology);
    }
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

  maybeSendWasmReady();
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

type SetAudioRingBufferMessage = {
  type: "setAudioRingBuffer";
  ringBuffer: SharedArrayBuffer | null;
  capacityFrames: number;
  channelCount: number;
  dstSampleRate: number;
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

// Audio output ring buffer attachment (AudioWorklet producer-side).
//
// The actual audio producer lives in different workers depending on runtime mode:
// - CPU worker: demo tone / mic loopback
// - IO worker: guest HDA device (real VM runs)
let audioOutRingBuffer: SharedArrayBuffer | null = null;
let audioOutViews: AudioWorkletRingBufferViews | null = null;
let audioOutCapacityFrames = 0;
let audioOutChannelCount = 0;
let audioOutDstSampleRate = 0;
let audioOutTelemetryActive = false;
let audioOutTelemetryNextMs = 0;

const AUDIO_OUT_TELEMETRY_INTERVAL_MS = 50;

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

  const dev = hdaDevice;
  if (dev) {
    dev.setMicRingBuffer(ringBuffer);
    if (ringBuffer && micSampleRate > 0) {
      dev.setCaptureSampleRateHz(micSampleRate);
    }
  }
}

function attachAudioRingBuffer(
  ringBuffer: SharedArrayBuffer | null,
  capacityFrames?: number,
  channelCount?: number,
  dstSampleRate?: number,
): void {
  const cap = (capacityFrames ?? 0) >>> 0;
  const cc = (channelCount ?? 0) >>> 0;
  const sr = (dstSampleRate ?? 0) >>> 0;

  if (ringBuffer !== null) {
    const Sab = globalThis.SharedArrayBuffer;
    if (typeof Sab === "undefined") {
      throw new Error("SharedArrayBuffer is unavailable; audio output requires crossOriginIsolated.");
    }
    if (!(ringBuffer instanceof Sab)) {
      throw new Error("setAudioRingBuffer expects a SharedArrayBuffer or null.");
    }
    // Validate against the canonical ring buffer layout (also creates convenient views).
    if (ringBuffer.byteLength < AUDIO_OUT_HEADER_BYTES) {
      throw new Error(`audio ring buffer is too small: need at least ${AUDIO_OUT_HEADER_BYTES} bytes`);
    }
  }

  audioOutRingBuffer = ringBuffer;
  audioOutViews = ringBuffer ? wrapAudioOutRingBuffer(ringBuffer, cap, cc) : null;
  audioOutCapacityFrames = cap;
  audioOutChannelCount = cc;
  audioOutDstSampleRate = sr;
  audioOutTelemetryNextMs = 0;

  // If the guest HDA device is active, attach/detach the ring buffer so the WASM-side
  // HDA controller can stream directly into the AudioWorklet output ring.
  const dev = hdaDevice;
  if (dev) {
    try {
      dev.setAudioRingBuffer({
        ringBuffer,
        capacityFrames: audioOutCapacityFrames,
        channelCount: audioOutChannelCount,
        dstSampleRateHz: audioOutDstSampleRate,
      });
    } catch (err) {
      console.warn("[io.worker] HDA setAudioRingBuffer failed:", err);
    }
  }
}

function maybePublishAudioOutTelemetry(nowMs: number): void {
  // The IO worker should only publish these counters when the guest HDA device is
  // active (i.e. during real VM runs). The CPU worker owns these counters during
  // demo tone / loopback mode.
  const views = audioOutViews;
  const capacityFrames = audioOutCapacityFrames;
  const hdaActive = !!currentConfig?.activeDiskImage;
  const shouldPublish = hdaActive && !!views && capacityFrames > 0;

  if (!shouldPublish) {
    if (audioOutTelemetryActive) {
      audioOutTelemetryActive = false;
      audioOutTelemetryNextMs = 0;
      Atomics.store(status, StatusIndex.AudioBufferLevelFrames, 0);
      Atomics.store(status, StatusIndex.AudioUnderrunCount, 0);
      Atomics.store(status, StatusIndex.AudioOverrunCount, 0);
    }
    return;
  }

  audioOutTelemetryActive = true;
  if (audioOutTelemetryNextMs !== 0 && nowMs < audioOutTelemetryNextMs) return;

  const bufferLevelFrames = getAudioOutRingBufferLevelFrames(views.header, capacityFrames);
  const underrunCount = Atomics.load(views.underrunCount, 0) >>> 0;
  const overrunCount = Atomics.load(views.overrunCount, 0) >>> 0;

  Atomics.store(status, StatusIndex.AudioBufferLevelFrames, bufferLevelFrames | 0);
  Atomics.store(status, StatusIndex.AudioUnderrunCount, underrunCount | 0);
  Atomics.store(status, StatusIndex.AudioOverrunCount, overrunCount | 0);

  audioOutTelemetryNextMs = nowMs + AUDIO_OUT_TELEMETRY_INTERVAL_MS;
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

async function handleVmSnapshotSaveToOpfs(path: string, cpu: ArrayBuffer, mmu: ArrayBuffer): Promise<void> {
  if (snapshotOpInFlight) {
    throw new Error("VM snapshot operation already in progress.");
  }
  snapshotOpInFlight = true;
  try {
    const api = wasmApi;
    if (!api) {
      throw new Error("WASM is not initialized in the IO worker; cannot save VM snapshot.");
    }

    const usb = snapshotUsbDeviceState();
    const ps2 = snapshotI8042DeviceState();
    const hda = snapshotAudioHdaDeviceState();
    const e1000 = snapshotE1000DeviceState();

    // Merge in any previously restored device blobs so unknown/unhandled device state survives a
    // restore → save cycle (forward compatibility).
    const freshDevices: Array<{ kind: string; bytes: Uint8Array }> = [];
    if (usb) freshDevices.push(usb);
    if (ps2) freshDevices.push(ps2);
    if (hda) freshDevices.push(hda);
    if (e1000) freshDevices.push(e1000);

    const freshKinds = new Set(freshDevices.map((d) => d.kind));
    const devices: Array<{ kind: string; bytes: Uint8Array }> = [];
    const seen = new Set<string>();
    for (const cached of snapshotRestoredDeviceBlobs) {
      if (freshKinds.has(cached.kind)) continue;
      if (seen.has(cached.kind)) continue;
      devices.push(cached);
      seen.add(cached.kind);
    }
    for (const dev of freshDevices) {
      if (seen.has(dev.kind)) continue;
      devices.push(dev);
      seen.add(dev.kind);
    }

    const saveExport = resolveVmSnapshotSaveToOpfsExport(api);
    if (!saveExport) {
      throw new Error("WASM VM snapshot save export is unavailable (expected *_snapshot*_to_opfs or WorkerVmSnapshot).");
    }

    if (saveExport.kind === "free-function") {
      // Build a JS-friendly device blob list; wasm-bindgen can accept this as `JsValue`.
      const devicePayload = devices.map((d) => ({ kind: d.kind, bytes: d.bytes }));

      // Always pass fresh Uint8Array views for the CPU state so callers can transfer the ArrayBuffer.
      const cpuBytes = new Uint8Array(cpu);
      const mmuBytes = new Uint8Array(mmu);

      await Promise.resolve(saveExport.fn.call(api as unknown, path, cpuBytes, mmuBytes, devicePayload));
      return;
    }

    const builder = new saveExport.Ctor(guestBase >>> 0, guestSize >>> 0);
    try {
      builder.set_cpu_state_v2(new Uint8Array(cpu), new Uint8Array(mmu));

      for (const device of devices) {
        const id = vmSnapshotDeviceKindToId(device.kind);
        if (id === null) {
          throw new Error(`Unsupported VM snapshot device kind: ${device.kind}`);
        }
        const { version, flags } = parseAeroIoSnapshotVersion(device.bytes);
        builder.add_device_state(id, version, flags, device.bytes);
      }

      await builder.snapshot_full_to_opfs(path);
    } finally {
      try {
        builder.free();
      } catch {
        // ignore
      }
    }
  } finally {
    snapshotOpInFlight = false;
  }
}

async function handleVmSnapshotRestoreFromOpfs(path: string): Promise<{
  cpu: ArrayBuffer;
  mmu: ArrayBuffer;
  devices?: VmSnapshotDeviceBlob[];
}> {
  if (snapshotOpInFlight) {
    throw new Error("VM snapshot operation already in progress.");
  }
  snapshotOpInFlight = true;
  try {
    const api = wasmApi;
    if (!api) {
      throw new Error("WASM is not initialized in the IO worker; cannot restore VM snapshot.");
    }

    const restoreExport = resolveVmSnapshotRestoreFromOpfsExport(api);
    if (!restoreExport) {
      throw new Error(
        "WASM VM snapshot restore export is unavailable (expected *_restore*_from_opfs or WorkerVmSnapshot).",
      );
    }

    if (restoreExport.kind === "free-function") {
      const res = await Promise.resolve(restoreExport.fn.call(api as unknown, path));
      const rec = res as { cpu?: unknown; mmu?: unknown; devices?: unknown };
      if (!(rec?.cpu instanceof Uint8Array) || !(rec?.mmu instanceof Uint8Array)) {
        throw new Error("WASM snapshot restore returned an unexpected result shape (expected {cpu:Uint8Array, mmu:Uint8Array}).");
      }

      const devicesRaw = Array.isArray(rec.devices) ? rec.devices : [];
      const devices: VmSnapshotDeviceBlob[] = [];
      const cachedDevices: Array<{ kind: string; bytes: Uint8Array }> = [];
      for (const entry of devicesRaw) {
        if (!entry || typeof entry !== "object") continue;
        const e = entry as { kind?: unknown; bytes?: unknown };
        if (typeof e.kind !== "string") continue;
        if (!(e.bytes instanceof Uint8Array)) continue;
        cachedDevices.push({ kind: e.kind, bytes: e.bytes });
        devices.push({ kind: e.kind, bytes: copyU8ToArrayBuffer(e.bytes) });
      }

      // Apply device state locally (IO worker owns USB + input/audio device instances).
      const usbBlob = devicesRaw.find(
        (entry): entry is { kind: string; bytes: Uint8Array } =>
          !!entry &&
          typeof (entry as { kind?: unknown }).kind === "string" &&
          (entry as { kind: string }).kind === VM_SNAPSHOT_DEVICE_USB_KIND &&
          (entry as { bytes?: unknown }).bytes instanceof Uint8Array,
      );
      const audioBlob = devicesRaw.find(
        (entry): entry is { kind: string; bytes: Uint8Array } =>
          !!entry &&
          typeof (entry as { kind?: unknown }).kind === "string" &&
          (entry as { kind: string }).kind === VM_SNAPSHOT_DEVICE_AUDIO_HDA_KIND &&
          (entry as { bytes?: unknown }).bytes instanceof Uint8Array,
      );
      const e1000Blob = devicesRaw.find(
        (entry): entry is { kind: string; bytes: Uint8Array } =>
          !!entry &&
          typeof (entry as { kind?: unknown }).kind === "string" &&
          (entry as { kind: string }).kind === VM_SNAPSHOT_DEVICE_E1000_KIND &&
          (entry as { bytes?: unknown }).bytes instanceof Uint8Array,
      );
      if (usbBlob) {
        restoreUsbDeviceState(usbBlob.bytes);
      }
      if (audioBlob) {
        restoreAudioHdaDeviceState(audioBlob.bytes);
      }

      const i8042Blob = devicesRaw.find(
        (entry): entry is { kind: string; bytes: Uint8Array } =>
          !!entry &&
          typeof (entry as { kind?: unknown }).kind === "string" &&
          (entry as { kind: string }).kind === VM_SNAPSHOT_DEVICE_I8042_KIND &&
          (entry as { bytes?: unknown }).bytes instanceof Uint8Array,
      );
      if (i8042Blob) {
        restoreI8042DeviceState(i8042Blob.bytes);
      }
      if (e1000Blob) {
        restoreE1000DeviceState(e1000Blob.bytes);
      }

      const e1000Blob = devicesRaw.find(
        (entry): entry is { kind: string; bytes: Uint8Array } =>
          !!entry &&
          typeof (entry as { kind?: unknown }).kind === "string" &&
          (entry as { kind: string }).kind === VM_SNAPSHOT_DEVICE_E1000_KIND &&
          (entry as { bytes?: unknown }).bytes instanceof Uint8Array,
      );
      if (e1000Blob) {
        restoreE1000DeviceState(e1000Blob.bytes);
      }
      snapshotRestoredDeviceBlobs = cachedDevices;

      return {
        cpu: copyU8ToArrayBuffer(rec.cpu),
        mmu: copyU8ToArrayBuffer(rec.mmu),
        devices: devices.length ? devices : undefined,
      };
    }

    const builder = new restoreExport.Ctor(guestBase >>> 0, guestSize >>> 0);
    try {
      const res = await builder.restore_snapshot_from_opfs(path);
      const rec = res as { cpu?: unknown; mmu?: unknown; devices?: unknown };
      if (!(rec?.cpu instanceof Uint8Array) || !(rec?.mmu instanceof Uint8Array) || !Array.isArray(rec.devices)) {
        throw new Error(
          "WASM snapshot restore returned an unexpected result shape (expected {cpu:Uint8Array, mmu:Uint8Array, devices:Array}).",
        );
      }

      const devices: VmSnapshotDeviceBlob[] = [];
      const cachedDevices: Array<{ kind: string; bytes: Uint8Array }> = [];
      let usbBytes: Uint8Array | null = null;
      let i8042Bytes: Uint8Array | null = null;
      let hdaBytes: Uint8Array | null = null;
      let e1000Bytes: Uint8Array | null = null;
      for (const entry of rec.devices) {
        if (!entry || typeof entry !== "object") {
          throw new Error(
            "WASM snapshot restore returned an unexpected devices entry (expected {id:number,version:number,flags:number,data:Uint8Array}).",
          );
        }
        const e = entry as { id?: unknown; version?: unknown; flags?: unknown; data?: unknown };
        if (
          typeof e.id !== "number" ||
          typeof e.version !== "number" ||
          typeof e.flags !== "number" ||
          !(e.data instanceof Uint8Array)
        ) {
          throw new Error(
            "WASM snapshot restore returned an unexpected devices entry shape (expected {id:number,version:number,flags:number,data:Uint8Array}).",
          );
        }

        const kind = vmSnapshotDeviceIdToKind(e.id);
        if (!kind) continue;

        if (kind === VM_SNAPSHOT_DEVICE_USB_KIND) {
          usbBytes = e.data;
        }
        if (kind === VM_SNAPSHOT_DEVICE_I8042_KIND) {
          i8042Bytes = e.data;
        }
        if (kind === VM_SNAPSHOT_DEVICE_AUDIO_HDA_KIND) {
          hdaBytes = e.data;
        }
        if (kind === VM_SNAPSHOT_DEVICE_E1000_KIND) {
          e1000Bytes = e.data;
        }
        cachedDevices.push({ kind, bytes: e.data });
        devices.push({ kind, bytes: copyU8ToArrayBuffer(e.data) });
      }

      if (usbBytes) {
        restoreUsbDeviceState(usbBytes);
      }
      if (i8042Bytes) {
        restoreI8042DeviceState(i8042Bytes);
      }
      if (hdaBytes) {
        restoreAudioHdaDeviceState(hdaBytes);
      }
      if (e1000Bytes) {
        restoreE1000DeviceState(e1000Bytes);
      }

      snapshotRestoredDeviceBlobs = cachedDevices;

      return {
        cpu: copyU8ToArrayBuffer(rec.cpu),
        mmu: copyU8ToArrayBuffer(rec.mmu),
        devices: devices.length ? devices : undefined,
      };
    } finally {
      try {
        builder.free();
      } catch {
        // ignore
      }
    }
  } finally {
    snapshotOpInFlight = false;
  }
}

async function initWorker(init: WorkerInitMessage): Promise<void> {
  perf.spanBegin("worker:boot");
  try {
    // Initialize the runtime event ring early so fatal errors during WASM init
    // can be surfaced via the coordinator's ring log (not just postMessage).
    //
    // `worker:init` will re-create the RingBuffer wrapper once the shared layout
    // views are established; this just ensures `pushEvent*()` has a best-effort
    // sink from the start of the boot sequence.
    role = init.role ?? "io";
    if (!eventRing) {
      try {
        const regions = ringRegionsForWorker(role);
        eventRing = new RingBuffer(init.controlSab, regions.event.byteOffset);
      } catch {
        // Ignore if the SAB isn't initialized yet; postMessage ERROR remains a fallback.
      }
    }

    await perf.spanAsync("wasm:init", async () => {
      try {
        const { api, variant } = await initWasmForContext({
          variant: init.wasmVariant ?? "auto",
          module: init.wasmModule,
          memory: init.guestMemory,
        });
        // Sanity-check that the coordinator-provided `guestMemory` is actually wired up as
        // the WASM module's linear memory (imported+exported memory build).
        //
        // Upcoming IO-worker devices (e.g. HDA audio) rely on DMA into shared guest RAM;
        // if the module is instantiated with a private memory, device models will silently
        // break. Fail fast with a clear error instead of running in a subtly corrupted mode.
        // Probe within guest RAM so we validate the exact region DMA-backed devices will access.
        // Use a distinct offset from the CPU worker probe so concurrent init cannot race on the
        // same 32-bit word and trigger false-negative wiring failures.
        const memProbeGuestOffset = 0x104;
        const guestBaseBytes = api.guest_ram_layout(0).guest_base >>> 0;
        assertWasmMemoryWiring({
          api,
          memory: init.guestMemory,
          linearOffset: guestBaseBytes + memProbeGuestOffset,
          context: "io.worker",
        });
        wasmApi = api;
        pendingWasmInit = { api, variant };
        usbHid = new api.UsbHidBridge();
        maybeInitUhciDevice();
        maybeInitVirtioNetDevice();
        if (!virtioNetDevice) maybeInitE1000Device();
        maybeInitVirtioInput();
        maybeInitHdaDevice();

        maybeInitWasmHidGuestBridge();
        if (!api.UhciRuntime && api.UsbPassthroughDemo && !usbDemo) {
          try {
            usbDemoApi = new api.UsbPassthroughDemo();
            usbDemo = new UsbPassthroughDemoRuntime({
              demo: usbDemoApi,
              postMessage: (msg: UsbActionMessage | UsbPassthroughDemoResultMessage) => {
                if (msg.type === "usb.demoResult" && msg.result.status !== "error") {
                  resetUsbDemoErrorDedup();
                }
                if (import.meta.env.DEV && msg.type === "usb.demoResult") {
                  if (msg.result.status === "success") {
                    const bytes = msg.result.data;
                    const isDeviceDescriptor = bytes.length >= 12 && bytes[0] === 18 && bytes[1] === 1;
                    const idVendor = isDeviceDescriptor ? bytes[8]! | (bytes[9]! << 8) : null;
                    const idProduct = isDeviceDescriptor ? bytes[10]! | (bytes[11]! << 8) : null;
                    console.log("[io.worker] WebUSB demo result ok", {
                      byteLength: bytes.byteLength,
                      head: Array.from(bytes.subarray(0, 64)),
                      idVendor,
                      idProduct,
                    });
                  } else {
                    console.log("[io.worker] WebUSB demo result", msg.result);
                  }
                }

                // `usb.demoResult` can contain a (potentially large) config descriptor payload.
                // Attempt to transfer the underlying buffer for the common case where it's a
                // standalone ArrayBuffer, but fall back to structured clone when the buffer is
                // non-transferable (e.g. a WebAssembly.Memory view).
                if (msg.type === "usb.demoResult" && msg.result.status === "success") {
                  const bytes = msg.result.data;
                  if (bytes.buffer instanceof ArrayBuffer && bytes.byteOffset === 0 && bytes.byteLength === bytes.buffer.byteLength) {
                    try {
                      ctx.postMessage(msg as unknown, [bytes.buffer]);
                      return;
                    } catch {
                      // fall through
                    }
                  }
                }

                ctx.postMessage(msg as unknown);
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
        if (err instanceof WasmMemoryWiringError) {
          console.error(`[io.worker] ${message}`);
          // Emit a log entry in addition to the fatal panic so callers inspecting the
          // runtime event stream (without special-casing `panic`) still see a clear,
          // actionable error explaining why the worker is terminating.
          pushEventBlocking({ kind: "log", level: "error", message });
          fatal(err);
          return;
        }
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
      ioIpcSab = segments.ioIpc;
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
        netTxRing = openRingByKind(segments.ioIpc, IO_IPC_NET_TX_QUEUE_KIND);
        netRxRing = openRingByKind(segments.ioIpc, IO_IPC_NET_RX_QUEUE_KIND);
      } catch {
        netTxRing = null;
        netRxRing = null;
      }
      try {
        hidInRing = openRingByKind(segments.ioIpc, IO_IPC_HID_IN_QUEUE_KIND);
      } catch {
        hidInRing = null;
      }

      // IRQ delivery between workers models *physical line levels* (asserted vs
      // deasserted) using discrete `irqRaise`/`irqLower` events (see
      // `docs/irq-semantics.md`).
      //
      // Multiple devices may share an IRQ line (e.g. PCI INTx). Model the
      // electrical wire-OR by keeping a refcount per line and only emitting
      // transitions:
      //   - emit `irqRaise` on 0→1
      //   - emit `irqLower` on 1→0
      //
      // Edge-triggered sources (e.g. i8042) are represented by emitting a pulse
      // (`raiseIrq()` then `lowerIrq()`); this refcounting ensures the pulse
      // reaches the CPU worker as a 0→1→0 transition (unless the line is already
      // asserted, which matches real hardware: you can't observe a rising edge
      // on an already-high line).
      //
      // Guardrails:
      //  - underflow is ignored (dev warning)
      //  - overflow saturates at 0xffff to avoid Uint16Array wraparound (dev warning)
      const irqRefCounts = new Uint16Array(256);
      const irqWarnedUnderflow = new Uint8Array(256);
      const irqWarnedSaturated = new Uint8Array(256);
      const irqSink: IrqSink = {
        raiseIrq: (irq) => {
          const idx = irq & 0xff;
          const flags = applyIrqRefCountChange(irqRefCounts, idx, true);
          if (flags & IRQ_REFCOUNT_ASSERT) enqueueIoEvent(encodeEvent({ kind: "irqRaise", irq: idx }));
          if (import.meta.env.DEV && (flags & IRQ_REFCOUNT_SATURATED) && irqWarnedSaturated[idx] === 0) {
            irqWarnedSaturated[idx] = 1;
            console.warn(`[io.worker] IRQ${idx} refcount saturated at 0xffff (raiseIrq without matching lowerIrq?)`);
          }
        },
        lowerIrq: (irq) => {
          const idx = irq & 0xff;
          const flags = applyIrqRefCountChange(irqRefCounts, idx, false);
          if (flags & IRQ_REFCOUNT_DEASSERT) enqueueIoEvent(encodeEvent({ kind: "irqLower", irq: idx }));
          if (import.meta.env.DEV && (flags & IRQ_REFCOUNT_UNDERFLOW) && irqWarnedUnderflow[idx] === 0) {
            irqWarnedUnderflow[idx] = 1;
            console.warn(`[io.worker] IRQ${idx} refcount underflow (lowerIrq while already deasserted)`);
          }
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

      // Prefer the canonical Rust i8042 model when the WASM export is available; fall back to
      // the legacy TS model for older/missing builds.
      i8042Ts = null;
      i8042Wasm?.free();
      i8042Wasm = null;
      const apiForI8042 = wasmApi;
      if (apiForI8042?.I8042Bridge) {
        try {
          const bridge = new apiForI8042.I8042Bridge();
          i8042Wasm = new I8042WasmController(bridge, mgr.irqSink, systemControl);
          mgr.registerPortIo(0x0060, 0x0060, i8042Wasm);
          mgr.registerPortIo(0x0064, 0x0064, i8042Wasm);
        } catch (err) {
          console.warn("[io.worker] Failed to initialize WASM I8042Bridge; falling back to TS i8042", err);
        }
      }

      if (!i8042Wasm) {
        i8042Ts = new I8042Controller(mgr.irqSink, { systemControl });
        mgr.registerPortIo(0x0060, 0x0060, i8042Ts);
        mgr.registerPortIo(0x0064, 0x0064, i8042Ts);
      }

      mgr.registerPciDevice(new PciTestDevice());
      maybeInitUhciDevice();
      maybeInitVirtioNetDevice();
      if (!virtioNetDevice) maybeInitE1000Device();
      maybeInitVirtioInput();
      maybeInitHdaDevice();

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
      | Partial<CoordinatorToWorkerSnapshotMessage>
      | Partial<InputBatchMessage>
      | Partial<SetBootDisksMessage>
      | Partial<SetMicrophoneRingBufferMessage>
      | Partial<SetAudioRingBufferMessage>
      | Partial<HidProxyMessage>
      | Partial<UsbRingDetachMessage>
      | Partial<UsbSelectedMessage>
      | Partial<UsbCompletionMessage>
      | Partial<UsbPassthroughDemoRunMessage>
      | Partial<UsbUhciHarnessStartMessage>
      | Partial<UsbUhciHarnessStopMessage>
      | Partial<HidAttachMessage>
      | Partial<HidInputReportMessage>
      | undefined;
    if (!data) return;

    const snapshotMsg = data as Partial<CoordinatorToWorkerSnapshotMessage>;
    if (typeof snapshotMsg.kind === "string" && snapshotMsg.kind.startsWith("vm.snapshot.")) {
      const requestId = snapshotMsg.requestId;
      if (typeof requestId !== "number") return;

      switch (snapshotMsg.kind) {
        case "vm.snapshot.pause": {
          snapshotPaused = true;
          setUsbProxyCompletionRingDispatchPaused(true);
          ctx.postMessage({ kind: "vm.snapshot.paused", requestId, ok: true } satisfies VmSnapshotPausedMessage);
          return;
        }
        case "vm.snapshot.resume": {
          snapshotPaused = false;
          flushQueuedInputBatches();
          flushQueuedSnapshotPausedMessages();
          setUsbProxyCompletionRingDispatchPaused(false);
          // Ensure the next device tick doesn't interpret wall-clock time spent in
          // snapshot save/restore as elapsed VM time.
          const now = typeof performance?.now === "function" ? performance.now() : Date.now();
          ioTickTimebase.resetHostNowMs(now);
          ctx.postMessage({ kind: "vm.snapshot.resumed", requestId, ok: true } satisfies VmSnapshotResumedMessage);
          return;
        }
        case "vm.snapshot.saveToOpfs": {
          void (async () => {
            try {
              if (!snapshotPaused) {
                throw new Error("IO worker is not paused; call vm.snapshot.pause before saving.");
              }
              if (typeof snapshotMsg.path !== "string") {
                throw new Error("vm.snapshot.saveToOpfs expected a string path.");
              }
              if (!(snapshotMsg.cpu instanceof ArrayBuffer) || !(snapshotMsg.mmu instanceof ArrayBuffer)) {
                throw new Error("vm.snapshot.saveToOpfs expected cpu/mmu ArrayBuffer payloads.");
              }
              await handleVmSnapshotSaveToOpfs(snapshotMsg.path, snapshotMsg.cpu, snapshotMsg.mmu);
              ctx.postMessage({ kind: "vm.snapshot.saved", requestId, ok: true } satisfies VmSnapshotSavedMessage);
            } catch (err) {
              ctx.postMessage({
                kind: "vm.snapshot.saved",
                requestId,
                ok: false,
                error: serializeVmSnapshotError(err),
              } satisfies VmSnapshotSavedMessage);
            }
          })();
          return;
        }
        case "vm.snapshot.restoreFromOpfs": {
          void (async () => {
            try {
              if (!snapshotPaused) {
                throw new Error("IO worker is not paused; call vm.snapshot.pause before restoring.");
              }
              if (typeof snapshotMsg.path !== "string") {
                throw new Error("vm.snapshot.restoreFromOpfs expected a string path.");
              }
              const restored = await handleVmSnapshotRestoreFromOpfs(snapshotMsg.path);
              const transfers: Transferable[] = [restored.cpu, restored.mmu];
              if (restored.devices) {
                for (const dev of restored.devices) transfers.push(dev.bytes);
              }
              ctx.postMessage(
                {
                  kind: "vm.snapshot.restored",
                  requestId,
                  ok: true,
                  cpu: restored.cpu,
                  mmu: restored.mmu,
                  devices: restored.devices,
                } satisfies VmSnapshotRestoredMessage,
                transfers,
              );
            } catch (err) {
              ctx.postMessage({
                kind: "vm.snapshot.restored",
                requestId,
                ok: false,
                error: serializeVmSnapshotError(err),
              } satisfies VmSnapshotRestoredMessage);
            }
          })();
          return;
        }
        default:
          return;
      }
    }

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

    if ((data as Partial<SetAudioRingBufferMessage>).type === "setAudioRingBuffer") {
      const msg = data as Partial<SetAudioRingBufferMessage>;
      attachAudioRingBuffer((msg.ringBuffer as SharedArrayBuffer | null) ?? null, msg.capacityFrames, msg.channelCount, msg.dstSampleRate);
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

    if (isUsbRingDetachMessage(data)) {
      // The SAB rings are an optional fast path. If they are detached (e.g. due to ring corruption),
      // clear the cached attach payload so newly constructed runtimes don't attempt to re-use the
      // stale ring handles.
      usbRingAttach = null;
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

    if (isUsbPassthroughDemoRunMessage(data)) {
      const msg = data;
      resetUsbDemoErrorDedup();
      if (!usbDemo) {
        emitUsbDemoError("UsbPassthroughDemo is unavailable in this WASM build.");
        return;
      }

      if (!lastUsbSelected?.ok) {
        const detail = lastUsbSelected?.error;
        const message = detail ? `No WebUSB device is selected: ${detail}` : "No WebUSB device is selected.";
        emitUsbDemoError(message);
        return;
      }

      try {
        usbDemo.run(msg.request, msg.length);
      } catch (err) {
        handleUsbDemoFailure("run", err);
      }
      return;
    }

    if (isUsbSelectedMessage(data)) {
      const msg = data;
      usbAvailable = msg.ok;
      lastUsbSelected = msg;
      if (webUsbGuestBridge) {
        try {
          applyUsbSelectedToWebUsbUhciBridge(webUsbGuestBridge, msg);
          if (uhciRuntimeWebUsbBridge && webUsbGuestBridge === uhciRuntimeWebUsbBridge) {
            webUsbGuestAttached = uhciRuntimeWebUsbBridge.is_connected();
            webUsbGuestLastError = uhciRuntimeWebUsbBridge.last_error();
          } else {
            webUsbGuestAttached = msg.ok;
            webUsbGuestLastError = null;
          }
        } catch (err) {
          console.warn("[io.worker] Failed to apply usb.selected to guest WebUSB bridge", err);
          webUsbGuestAttached = false;
          webUsbGuestLastError = `Failed to apply usb.selected to guest WebUSB bridge: ${formatWebUsbGuestError(err)}`;
        }
      } else {
        webUsbGuestAttached = false;
        if (!msg.ok) {
          webUsbGuestLastError = null;
        } else if (wasmApi && !wasmApi.UhciControllerBridge && !wasmApi.UhciRuntime) {
          webUsbGuestLastError =
            "UhciControllerBridge export unavailable (guest-visible WebUSB passthrough unsupported in this WASM build).";
        } else {
          webUsbGuestLastError = null;
        }
      }
      if (usbDemo) {
        try {
          if (msg.ok) resetUsbDemoErrorDedup();
          usbDemo.onUsbSelected(msg);
        } catch (err) {
          if (msg.ok) {
            handleUsbDemoFailure("onUsbSelected", err);
          } else {
            console.warn("[io.worker] UsbPassthroughDemo.onUsbSelected failed", err);
            try {
              usbDemo.reset();
            } catch {
              // ignore
            }
          }
        }
      }
      emitWebUsbGuestStatus();

      // Dev-only smoke test: once a device is selected on the main thread, request the
      // first 18 bytes of the device descriptor to prove the cross-thread broker works.
      if (msg.ok && import.meta.env.DEV && !usbDemo && !wasmApi?.UhciRuntime) {
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

    if (isUsbCompletionMessage(data)) {
      const msg = data;
      if (usbDemo) {
        try {
          usbDemo.onUsbCompletion(msg);
        } catch (err) {
          handleUsbDemoFailure("onUsbCompletion", err);
        }
      }
      if (import.meta.env.DEV) {
        if (msg.completion.status === "success" && "data" in msg.completion) {
          const data = msg.completion.data;
          console.log("[io.worker] WebUSB completion success", msg.completion.kind, msg.completion.id, {
            byteLength: data.byteLength,
            head: Array.from(data.subarray(0, 64)),
          });
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
      const recycle = (msg as { recycle?: unknown }).recycle === true;

      // Snapshot pause must freeze device-side state so the snapshot contents are deterministic.
      // Queue input while paused and replay after `vm.snapshot.resume`.
      if (snapshotPaused) {
        if (queuedInputBatchBytes + buffer.byteLength <= MAX_QUEUED_INPUT_BATCH_BYTES) {
          queuedInputBatches.push({ buffer, recycle });
          queuedInputBatchBytes += buffer.byteLength;
        } else {
          // Drop excess input to keep memory bounded; best-effort recycle the transferred buffer.
          if (recycle) {
            ctx.postMessage({ type: "in:input-batch-recycle", buffer } satisfies InputBatchRecycleMessage, [buffer]);
          }
        }
        return;
      }
      if (started) {
        handleInputBatch(buffer);
      }
      if (recycle) {
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
      const vmNowMs = ioTickTimebase.tick(nowMs, snapshotPaused);

      const perfActive = isPerfActive();
      const t0 = perfActive ? performance.now() : 0;

      // Snapshot pause: freeze device-side state so the coordinator can take a
      // consistent CPU + RAM + device snapshot. Keep draining the runtime control
      // ring so shutdown requests are still observed, but avoid ticking devices.
      if (snapshotPaused) {
        drainRuntimeCommands();
        if (Atomics.load(status, StatusIndex.StopRequested) === 1) {
          ioServerAbort?.abort();
        }
        return;
      }

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
      mgr.tick(vmNowMs);
      flushSyntheticUsbHidPendingInputReports();
      maybeUpdateKeyboardInputBackend({ virtioKeyboardOk: virtioInputKeyboard?.driverOk() ?? false });
      maybeUpdateMouseInputBackend({ virtioMouseOk: virtioInputMouse?.driverOk() ?? false });
      drainSyntheticUsbHidOutputReports();
      hidGuest.poll?.();
      void usbPassthroughRuntime?.pollOnce();
      usbUhciHarnessRuntime?.pollOnce();
      if (usbDemo) {
        try {
          usbDemo.tick();
          usbDemo.pollResults();
        } catch (err) {
          handleUsbDemoFailure("tick", err);
        }
      }

      // Publish AudioWorklet-ring producer telemetry when the IO worker is acting
      // as the audio producer (guest HDA device in VM mode).
      maybePublishAudioOutTelemetry(nowMs);

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

function updatePressedKeyboardHidUsage(usage: number, pressed: boolean): void {
  const u = usage & 0xff;
  const prev = pressedKeyboardHidUsages[u] ?? 0;
  if (pressed) {
    if (prev === 0) {
      pressedKeyboardHidUsages[u] = 1;
      pressedKeyboardHidUsageCount += 1;
    }
    return;
  }
  if (prev !== 0) {
    pressedKeyboardHidUsages[u] = 0;
    pressedKeyboardHidUsageCount = Math.max(0, pressedKeyboardHidUsageCount - 1);
  }
}

function maybeUpdateKeyboardInputBackend(opts: { virtioKeyboardOk: boolean }): void {
  keyboardInputBackend = chooseKeyboardInputBackend({
    current: keyboardInputBackend,
    keysHeld: pressedKeyboardHidUsageCount !== 0,
    virtioOk: opts.virtioKeyboardOk && !!virtioInputKeyboard,
    usbOk: syntheticUsbHidAttached && !!usbHid && safeSyntheticUsbHidConfigured(syntheticUsbKeyboard),
  });
}

function maybeUpdateMouseInputBackend(opts: { virtioMouseOk: boolean }): void {
  mouseInputBackend = chooseMouseInputBackend({
    current: mouseInputBackend,
    buttonsHeld: (mouseButtonsMask & 0x07) !== 0,
    virtioOk: opts.virtioMouseOk && !!virtioInputMouse,
    // Prefer PS/2 mouse injection whenever an i8042 controller is available so mouse input works
    // even when the synthetic USB HID mouse is absent/unconfigured.
    usbOk: !(i8042Wasm || i8042Ts),
  });
}

function drainSyntheticUsbHidReports(): void {
  const source = usbHid;
  if (!source) return;

  // Lazy-init so older WASM builds (or unit tests) can run without the UHCI/hid-passthrough exports.
  maybeInitSyntheticUsbHidDevices();

  const keyboard = syntheticUsbKeyboard;
  const mouse = syntheticUsbMouse;
  const gamepad = syntheticUsbGamepad;

  const keyboardConfigured = safeSyntheticUsbHidConfigured(keyboard);
  if (keyboardConfigured && keyboard && syntheticUsbKeyboardPendingReport) {
    try {
      keyboard.push_input_report(0, syntheticUsbKeyboardPendingReport);
    } catch {
      // ignore
    }
    syntheticUsbKeyboardPendingReport = null;
  }
  for (let i = 0; i < MAX_SYNTHETIC_USB_HID_REPORTS_PER_INPUT_BATCH; i += 1) {
    let report: Uint8Array | null = null;
    try {
      report = source.drain_next_keyboard_report();
    } catch {
      break;
    }
    if (!(report instanceof Uint8Array)) break;
    if (keyboardConfigured && keyboard) {
      try {
        keyboard.push_input_report(0, report);
      } catch {
        // ignore
      }
    } else {
      // For stateful devices like the keyboard, keep the latest report so we can send it once the
      // guest configures the device. This avoids the "held key during enumeration" edge case
      // where the guest never sees the pressed state.
      syntheticUsbKeyboardPendingReport = report;
    }
  }

  const mouseConfigured = safeSyntheticUsbHidConfigured(mouse);
  for (let i = 0; i < MAX_SYNTHETIC_USB_HID_REPORTS_PER_INPUT_BATCH; i += 1) {
    let report: Uint8Array | null = null;
    try {
      report = source.drain_next_mouse_report();
    } catch {
      break;
    }
    if (!(report instanceof Uint8Array)) break;
    if (!mouseConfigured || !mouse) continue;
    try {
      mouse.push_input_report(0, report);
    } catch {
      // ignore
    }
  }

  const gamepadConfigured = safeSyntheticUsbHidConfigured(gamepad);
  if (gamepadConfigured && gamepad && syntheticUsbGamepadPendingReport) {
    try {
      gamepad.push_input_report(0, syntheticUsbGamepadPendingReport);
    } catch {
      // ignore
    }
    syntheticUsbGamepadPendingReport = null;
  }
  for (let i = 0; i < MAX_SYNTHETIC_USB_HID_REPORTS_PER_INPUT_BATCH; i += 1) {
    let report: Uint8Array | null = null;
    try {
      report = source.drain_next_gamepad_report();
    } catch {
      break;
    }
    if (!(report instanceof Uint8Array)) break;
    if (gamepadConfigured && gamepad) {
      try {
        gamepad.push_input_report(0, report);
      } catch {
        // ignore
      }
    } else {
      syntheticUsbGamepadPendingReport = report;
    }
  }
}

function flushSyntheticUsbHidPendingInputReports(): void {
  // Lazy-init so older WASM builds (or unit tests) can run without the UHCI/hid-passthrough exports.
  maybeInitSyntheticUsbHidDevices();

  const keyboard = syntheticUsbKeyboard;
  if (keyboard && syntheticUsbKeyboardPendingReport && safeSyntheticUsbHidConfigured(keyboard)) {
    try {
      keyboard.push_input_report(0, syntheticUsbKeyboardPendingReport);
    } catch {
      // ignore
    }
    syntheticUsbKeyboardPendingReport = null;
  }

  const gamepad = syntheticUsbGamepad;
  if (gamepad && syntheticUsbGamepadPendingReport && safeSyntheticUsbHidConfigured(gamepad)) {
    try {
      gamepad.push_input_report(0, syntheticUsbGamepadPendingReport);
    } catch {
      // ignore
    }
    syntheticUsbGamepadPendingReport = null;
  }
}

function drainSyntheticUsbHidOutputReports(): void {
  // Lazy-init so older WASM builds (or unit tests) can run without the UHCI/hid-passthrough exports.
  maybeInitSyntheticUsbHidDevices();

  const keyboard = syntheticUsbKeyboard;
  const mouse = syntheticUsbMouse;
  const gamepad = syntheticUsbGamepad;

  if (keyboard) drainSyntheticUsbHidOutputReportsForDevice(keyboard);
  if (mouse) drainSyntheticUsbHidOutputReportsForDevice(mouse);
  if (gamepad) drainSyntheticUsbHidOutputReportsForDevice(gamepad);
}

function drainSyntheticUsbHidOutputReportsForDevice(dev: UsbHidPassthroughBridge): void {
  if (!safeSyntheticUsbHidConfigured(dev)) return;
  for (let i = 0; i < MAX_SYNTHETIC_USB_HID_OUTPUT_REPORTS_PER_TICK; i += 1) {
    let report: unknown;
    try {
      report = dev.drain_next_output_report();
    } catch {
      break;
    }
    if (report == null) break;
  }
}

function safeSyntheticUsbHidConfigured(dev: UsbHidPassthroughBridge | null): boolean {
  if (!dev) return false;
  try {
    return dev.configured();
  } catch {
    return false;
  }
}

function handleInputBatch(buffer: ArrayBuffer): void {
  const t0 = performance.now();
  // `buffer` is transferred from the main thread, so it is uniquely owned here.
  const words = new Int32Array(buffer);
  const count = words[0] >>> 0;

  Atomics.add(status, StatusIndex.IoInputBatchCounter, 1);
  Atomics.add(status, StatusIndex.IoInputEventCounter, count);

  const virtioKeyboard = virtioInputKeyboard;
  const virtioMouse = virtioInputMouse;
  const virtioKeyboardOk = virtioKeyboard?.driverOk() ?? false;
  const virtioMouseOk = virtioMouse?.driverOk() ?? false;

  // Ensure synthetic USB HID devices exist (when supported) before processing this batch so we
  // can consistently decide whether to use the legacy PS/2 scancode injection path.
  maybeInitSyntheticUsbHidDevices();
  maybeUpdateKeyboardInputBackend({ virtioKeyboardOk });
  maybeUpdateMouseInputBackend({ virtioMouseOk });

  const base = 2;
  for (let i = 0; i < count; i++) {
    const off = base + i * 4;
    const type = words[off] >>> 0;
    switch (type) {
      case InputEventType.KeyHidUsage: {
        const packed = words[off + 2] >>> 0;
        const usage = packed & 0xff;
        const pressed = ((packed >>> 8) & 1) !== 0;
        updatePressedKeyboardHidUsage(usage, pressed);
        if (keyboardInputBackend === "virtio") {
          if (virtioKeyboardOk && virtioKeyboard) {
            const keyCode = hidUsageToLinuxKeyCode(usage);
            if (keyCode !== null) {
              virtioKeyboard.injectKey(keyCode, pressed);
            }
          }
        } else if (keyboardInputBackend === "usb") {
          usbHid?.keyboard_event(usage, pressed);
        }
        break;
      }
      case InputEventType.MouseMove: {
        const dx = words[off + 2] | 0;
        const dyPs2 = words[off + 3] | 0;
        if (mouseInputBackend === "virtio") {
          if (virtioMouseOk && virtioMouse) {
            // Input batches use PS/2 convention: positive = up. virtio-input uses Linux REL_Y where positive = down.
            virtioMouse.injectRelMove(dx, -dyPs2);
          }
        } else if (mouseInputBackend === "ps2") {
          if (i8042Wasm) {
            i8042Wasm.injectMouseMove(dx, dyPs2);
          } else if (i8042Ts) {
            i8042Ts.injectMouseMove(dx, dyPs2);
          }
        } else {
          // PS/2 convention: positive is up. HID convention: positive is down.
          usbHid?.mouse_move(dx, -dyPs2);
        }
        break;
      }
      case InputEventType.MouseButtons: {
        const buttons = words[off + 2] & 0xff;
        mouseButtonsMask = buttons & 0x07;
        if (mouseInputBackend === "virtio") {
          if (virtioMouseOk && virtioMouse) {
            virtioMouse.injectMouseButtons(buttons);
          }
        } else if (mouseInputBackend === "ps2") {
          if (i8042Wasm) {
            i8042Wasm.injectMouseButtons(buttons);
          } else if (i8042Ts) {
            i8042Ts.injectMouseButtons(buttons);
          }
        } else {
          usbHid?.mouse_buttons(buttons);
        }
        break;
      }
      case InputEventType.MouseWheel: {
        const dz = words[off + 2] | 0;
        if (mouseInputBackend === "virtio") {
          if (virtioMouseOk && virtioMouse) {
            virtioMouse.injectWheel(dz);
          }
        } else if (mouseInputBackend === "ps2") {
          if (i8042Wasm) {
            i8042Wasm.injectMouseWheel(dz);
          } else if (i8042Ts) {
            i8042Ts.injectMouseWheel(dz);
          }
        } else {
          usbHid?.mouse_wheel(dz);
        }
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
        // Only inject PS/2 scancodes when the PS/2 keyboard backend is active. Other backends
        // (synthetic USB HID / virtio-input) rely on `KeyHidUsage` events and would otherwise
        // cause duplicated input in the guest.
        if (keyboardInputBackend === "ps2") {
          if (i8042Wasm) {
            i8042Wasm.injectKeyScancode(packed, len);
          } else if (i8042Ts) {
            const bytes = new Uint8Array(len);
            for (let j = 0; j < len; j++) {
              bytes[j] = (packed >>> (j * 8)) & 0xff;
            }
            i8042Ts.injectKeyboardBytes(bytes);
          }
        }
        break;
      }
      default:
        // Unknown event type; ignore.
        break;
    }
  }

  // Re-evaluate backend selection after processing this batch; key-up events can make it safe to
  // transition away from PS/2 scancode injection.
  maybeUpdateKeyboardInputBackend({ virtioKeyboardOk });
  maybeUpdateMouseInputBackend({ virtioMouseOk });

  // Forward newly queued USB HID reports into the guest-visible UHCI USB HID devices.
  drainSyntheticUsbHidReports();

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

      syntheticUsbKeyboard?.free();
      syntheticUsbKeyboard = null;
      syntheticUsbMouse?.free();
      syntheticUsbMouse = null;
      syntheticUsbGamepad?.free();
      syntheticUsbGamepad = null;
      syntheticUsbHidAttached = false;
      syntheticUsbKeyboardPendingReport = null;
      syntheticUsbGamepadPendingReport = null;
      keyboardInputBackend = "ps2";
      pressedKeyboardHidUsages.fill(0);
      pressedKeyboardHidUsageCount = 0;
      mouseInputBackend = "ps2";
      mouseButtonsMask = 0;

      webUsbGuestBridge = null;

      if (usbPassthroughRuntime) {
        usbPassthroughRuntime.destroy();
        usbPassthroughRuntime = null;
      }

      usbUhciHarnessRuntime?.destroy();
      usbUhciHarnessRuntime = null;
      uhciDevice?.destroy();
      uhciDevice = null;
      virtioNetDevice?.destroy();
      virtioNetDevice = null;
      uhciControllerBridge = null;
      e1000Device?.destroy();
      e1000Device = null;
      e1000Bridge = null;
      virtioInputKeyboard?.destroy();
      virtioInputKeyboard = null;
      virtioInputMouse?.destroy();
      virtioInputMouse = null;
      uhciHidTopology.setUhciBridge(null);
      hdaDevice?.destroy();
      hdaDevice = null;
      hdaControllerBridge = null;
      try {
        usbDemoApi?.free();
      } catch {
        // ignore
      }
      usbDemoApi = null;
      usbDemo = null;
      lastUsbSelected = null;
      netTxRing = null;
      netRxRing = null;
      deviceManager = null;
      ioIpcSab = null;
      i8042Ts = null;
      i8042Wasm?.free();
      i8042Wasm = null;
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
