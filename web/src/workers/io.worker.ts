/// <reference lib="webworker" />

import type { AeroConfig } from "../config/aero_config";
import { PCI_MMIO_BASE, VRAM_BASE_PADDR } from "../arch/guest_phys.ts";
import { openRingByKind } from "../ipc/ipc";
import { decodeCommand, encodeEvent, type Command, type Event } from "../ipc/protocol";
import { RingBuffer } from "../ipc/ring_buffer";
import { InputEventType } from "../input/event_queue";
import { negateI32Saturating } from "../input/int32";
import { chooseKeyboardInputBackend, chooseMouseInputBackend, type InputBackend } from "../input/input_backend_selection";
import { encodeInputBackendStatus } from "../input/input_backend_status";
import { u32Delta } from "../utils/u32";
import { perf } from "../perf/perf";
import { installWorkerPerfHandlers } from "../perf/worker";
import { PerfWriter } from "../perf/writer.js";
import { PERF_FRAME_HEADER_ENABLED_INDEX, PERF_FRAME_HEADER_FRAME_ID_INDEX } from "../perf/shared.js";
import { initWasmForContext, type WasmApi, type WasmVariant } from "../runtime/wasm_context";
import { assertWasmMemoryWiring, WasmMemoryWiringError } from "../runtime/wasm_memory_probe";
import {
  layoutFromHeader,
  SHARED_FRAMEBUFFER_HEADER_U32_LEN,
  SharedFramebufferHeaderIndex,
  SHARED_FRAMEBUFFER_MAGIC,
  SHARED_FRAMEBUFFER_VERSION,
} from "../ipc/shared-layout";
import {
  serializeVmSnapshotError,
  type CoordinatorToWorkerSnapshotMessage,
  type VmSnapshotDeviceBlob,
  type VmSnapshotPausedMessage,
  type VmSnapshotResumedMessage,
  type VmSnapshotRestoredMessage,
  type VmSnapshotSavedMessage,
} from "../runtime/snapshot_protocol";
import { normalizeSetBootDisksMessage, type SetBootDisksMessage } from "../runtime/boot_disks_protocol";
import {
  IO_IPC_CMD_QUEUE_KIND,
  IO_IPC_EVT_QUEUE_KIND,
  IO_IPC_HID_IN_QUEUE_KIND,
  IO_IPC_NET_RX_QUEUE_KIND,
  IO_IPC_NET_TX_QUEUE_KIND,
  HIGH_RAM_START,
  LOW_RAM_END,
  StatusIndex,
  STATUS_OFFSET_BYTES,
  STATUS_INTS,
  createSharedMemoryViews,
  guestPaddrToRamOffset,
  guestRangeInBounds,
  ringRegionsForWorker,
  setReadyFlag,
  type GuestRamLayout,
  type WorkerRole,
} from "../runtime/shared_layout";
import {
  type ConfigAckMessage,
  type ConfigUpdateMessage,
  type CursorSetImageMessage,
  type CursorSetStateMessage,
  MessageType,
  type ProtocolMessage,
  type WorkerInitMessage,
} from "../runtime/protocol";
import { DeviceManager, type IrqSink } from "../io/device_manager";
import { MmioRamHandler } from "../io/bus/mmio_ram";
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
import { AeroGpuPciDevice } from "../io/devices/aerogpu";
import { UhciPciDevice, type UhciControllerBridgeLike } from "../io/devices/uhci";
import { EhciPciDevice, type EhciControllerBridgeLike } from "../io/devices/ehci";
import { XhciPciDevice } from "../io/devices/xhci";
import {
  VirtioInputPciFunction,
  hidConsumerUsageToLinuxKeyCode,
  hidUsageToLinuxKeyCode,
  type VirtioInputPciDeviceLike,
} from "../io/devices/virtio_input";
import { VirtioNetPciDevice } from "../io/devices/virtio_net";
import { VirtioSndPciDevice } from "../io/devices/virtio_snd";
import { UART_COM1, Uart16550, type SerialOutputSink } from "../io/devices/uart16550";
import { AeroIpcIoServer, type AeroIpcIoDiskResult, type AeroIpcIoDispatchTarget } from "../io/ipc/aero_ipc_io";
import { defaultReadValue } from "../io/ipc/io_protocol";
import { RuntimeDiskClient } from "../storage/runtime_disk_client";
import { computeAlignedDiskIoRange, diskReadIntoGuest, diskWriteFromGuest } from "./io_disk_dma";
import {
  isUsbRingAttachMessage,
  isUsbRingDetachMessage,
  isUsbCompletionMessage,
  isUsbGuestControllerModeMessage,
  isUsbSelectedMessage,
  type UsbActionMessage,
  type UsbCompletionMessage,
  type UsbGuestControllerMode,
  type UsbGuestWebUsbSnapshot,
  type UsbGuestWebUsbStatusMessage,
  type UsbHostAction,
  type UsbHostCompletion,
  type UsbRingAttachMessage,
  type UsbRingDetachMessage,
  type UsbSelectedMessage,
} from "../usb/usb_proxy_protocol";
import { setUsbProxyCompletionRingDispatchPaused } from "../usb/usb_proxy_ring_dispatcher";
import { applyUsbSelectedToWebUsbUhciBridge } from "../usb/uhci_webusb_bridge";
import { createUsbBrokerSubportNoOtherSpeedTranslation } from "../usb/usb_broker_subport";
import type { UsbUhciHarnessStartMessage, UsbUhciHarnessStatusMessage, UsbUhciHarnessStopMessage, WebUsbUhciHarnessRuntimeSnapshot } from "../usb/webusb_harness_runtime";
import { WebUsbUhciHarnessRuntime } from "../usb/webusb_harness_runtime";
import type {
  UsbEhciHarnessStatusMessage,
  WebUsbEhciHarnessRuntimeSnapshot,
  UsbEhciHarnessAttachControllerMessage,
  UsbEhciHarnessDetachControllerMessage,
  UsbEhciHarnessAttachDeviceMessage,
  UsbEhciHarnessDetachDeviceMessage,
  UsbEhciHarnessGetDeviceDescriptorMessage,
  UsbEhciHarnessGetConfigDescriptorMessage,
  UsbEhciHarnessClearUsbStsMessage,
} from "../usb/webusb_ehci_harness_runtime";
import { WebUsbEhciHarnessRuntime } from "../usb/webusb_ehci_harness_runtime";
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
  type HidAttachResultMessage,
  isHidDetachMessage,
  isHidFeatureReportResultMessage,
  isHidInputReportMessage,
  type HidGetFeatureReportMessage,
  isHidProxyMessage,
  isHidRingAttachMessage,
  isHidRingDetachMessage,
  isHidRingInitMessage,
  type HidAttachMessage,
  type HidDetachMessage,
  type HidErrorMessage,
  type HidFeatureReportResultMessage,
  type HidInputReportMessage,
  type HidLogMessage,
  type HidProxyMessage,
  type HidRingAttachMessage,
  type HidRingDetachMessage,
  type HidRingInitMessage,
} from "../hid/hid_proxy_protocol";
import { InMemoryHidGuestBridge } from "../hid/in_memory_hid_guest_bridge";
import { createEhciTopologyBridgeShim } from "../hid/ehci_hid_topology_shim";
import { UhciHidTopologyManager, type UhciTopologyBridge } from "../hid/uhci_hid_topology";
import { XhciHidTopologyManager, type XhciTopologyBridge } from "../hid/xhci_hid_topology";
import { WasmHidGuestBridge, type HidGuestBridge, type HidHostSink } from "../hid/wasm_hid_guest_bridge";
import { WasmUhciHidGuestBridge } from "../hid/wasm_uhci_hid_guest_bridge";
import {
  HEADER_BYTES as AUDIO_OUT_HEADER_BYTES,
  getRingBufferLevelFrames as getAudioOutRingBufferLevelFrames,
  wrapRingBuffer as wrapAudioOutRingBuffer,
  type AudioWorkletRingBufferViews,
} from "../audio/audio_worklet_ring";
import {
  CAPACITY_SAMPLES_INDEX as MIC_CAPACITY_SAMPLES_INDEX,
  HEADER_BYTES as MIC_HEADER_BYTES,
  HEADER_U32_LEN as MIC_HEADER_U32_LEN,
} from "../audio/mic_ring.js";
import {
  isHidAttachHubMessage as isHidPassthroughAttachHubMessage,
  isHidAttachMessage as isHidPassthroughAttachMessage,
  isHidDetachMessage as isHidPassthroughDetachMessage,
  isHidFeatureReportResultMessage as isHidPassthroughFeatureReportResultMessage,
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
  USB_HID_CONSUMER_CONTROL_REPORT_DESCRIPTOR,
  USB_HID_GAMEPAD_REPORT_DESCRIPTOR,
  USB_HID_INTERFACE_PROTOCOL_KEYBOARD,
  USB_HID_INTERFACE_PROTOCOL_MOUSE,
  USB_HID_INTERFACE_SUBCLASS_BOOT,
} from "../usb/hid_descriptors";
import {
  EXTERNAL_HUB_ROOT_PORT,
  DEFAULT_EXTERNAL_HUB_PORT_COUNT,
  WEBUSB_GUEST_ROOT_PORT,
  UHCI_SYNTHETIC_HID_GAMEPAD_HUB_PORT,
  UHCI_SYNTHETIC_HID_CONSUMER_CONTROL_HUB_PORT,
  UHCI_SYNTHETIC_HID_KEYBOARD_HUB_PORT,
  UHCI_SYNTHETIC_HID_MOUSE_HUB_PORT,
} from "../usb/uhci_external_hub";
import { IoWorkerLegacyHidPassthroughAdapter } from "./io_hid_passthrough_legacy_adapter";
import { createXhciTopologyBridgeShim, IoWorkerHidTopologyMux } from "./io_hid_topology_mux";
import { drainIoHidInputRing } from "./io_hid_input_ring";
import { forwardHidSendReportToMainThread } from "./io_hid_output_report_forwarding";
import { restoreIoWorkerVmSnapshotFromOpfs, saveIoWorkerVmSnapshotToOpfs } from "./io_worker_vm_snapshot";
import { UhciRuntimeExternalHubConfigManager } from "./uhci_runtime_hub_config";
import { applyUsbSelectedToWebUsbGuestBridge, chooseWebUsbGuestBridge, type WebUsbGuestControllerKind } from "./io_webusb_guest_selection";
import {
  VM_SNAPSHOT_DEVICE_AUDIO_HDA_KIND,
  VM_SNAPSHOT_DEVICE_AUDIO_VIRTIO_SND_KIND,
  VM_SNAPSHOT_DEVICE_E1000_KIND,
  VM_SNAPSHOT_DEVICE_USB_KIND,
} from "./vm_snapshot_wasm";
import {
  IO_WORKER_RUNTIME_DISK_SNAPSHOT_KIND,
  findRuntimeDiskWorkerSnapshotDeviceBlob,
  restoreRuntimeDiskWorkerSnapshotFromDeviceBlobs,
} from "./io_worker_runtime_disk_snapshot";
import { pauseIoWorkerSnapshotAndDrainDiskIo } from "./io_worker_snapshot_pause";
import { tryInitVirtioNetDevice } from "./io_virtio_net_init";
import { tryInitVirtioSndDevice } from "./io_virtio_snd_init";
import { tryInitXhciDevice } from "./io_xhci_init";
import { registerVirtioInputKeyboardPciFunction } from "./io_virtio_input_register";
import { VmTimebase } from "../runtime/vm_timebase";
import {
  INPUT_BATCH_HEADER_BYTES,
  INPUT_BATCH_HEADER_WORDS,
  INPUT_BATCH_WORDS_PER_EVENT,
  MAX_INPUT_EVENTS_PER_BATCH,
  validateInputBatchBuffer,
} from "./io_input_batch";
import { MAX_INPUT_BATCH_RECYCLE_BYTES, shouldRecycleInputBatchBuffer } from "./input_batch_recycle_guard";

const ctx = self as unknown as DedicatedWorkerGlobalScope;

// `import.meta.env` is injected by Vite for browser builds, but is undefined when
// running the worker entrypoints directly under Node (e.g. worker_threads tests).
const IS_DEV = (import.meta as unknown as { env?: { DEV?: unknown } }).env?.DEV === true;

void installWorkerPerfHandlers();

type InputBatchMessage = { type: "in:input-batch"; buffer: ArrayBuffer };
type InputBatchRecycleMessage = { type: "in:input-batch-recycle"; buffer: ArrayBuffer };

let role: WorkerRole = "io";
let status!: Int32Array;
let guestU8!: Uint8Array;
let vramU8: Uint8Array | null = null;
let vramBasePaddr = 0;
let vramSizeBytes = 0;
let guestBase = 0;
let guestSize = 0;
let guestLayout: GuestRamLayout | null = null;
let sharedFramebuffer: { sab: SharedArrayBuffer; offsetBytes: number } | null = null;

function rangesOverlap(aStart: number, aLen: number, bStart: number, bLen: number): boolean {
  const aEnd = aStart + aLen;
  const bEnd = bStart + bLen;
  return aStart < bEnd && bStart < aEnd;
}

function assertNoGuestOverlapWithSharedFramebuffer(guestOffset: number, len: number, label: string): void {
  const shared = sharedFramebuffer;
  if (!shared) return;
  // Only relevant when the shared framebuffer is embedded in guest RAM (it may fall back to a dedicated
  // SharedArrayBuffer when guest memory is tiny).
  if (shared.sab !== (guestU8.buffer as unknown as SharedArrayBuffer)) return;
  const base = guestBase >>> 0;
  if (shared.offsetBytes < base) return;
  const sharedGuestOffset = (shared.offsetBytes - base) >>> 0;

  const header = new Int32Array(shared.sab, shared.offsetBytes, SHARED_FRAMEBUFFER_HEADER_U32_LEN);
  const magic = Atomics.load(header, SharedFramebufferHeaderIndex.MAGIC) | 0;
  const version = Atomics.load(header, SharedFramebufferHeaderIndex.VERSION) | 0;
  if (magic !== SHARED_FRAMEBUFFER_MAGIC || version !== SHARED_FRAMEBUFFER_VERSION) {
    // If the header is invalid, do not guess at the layout; failing later with the normal
    // shared framebuffer consumer path will be clearer.
    return;
  }
  const layout = layoutFromHeader(header);

  if (rangesOverlap(guestOffset, len, sharedGuestOffset, layout.totalBytes)) {
    throw new Error(
      `${label} guest range overlaps embedded shared framebuffer region: ` +
        `[0x${guestOffset.toString(16)}, +0x${len.toString(16)}] intersects ` +
        `[0x${sharedGuestOffset.toString(16)}, +0x${layout.totalBytes.toString(16)}]`,
    );
  }
}

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
let invalidInputBatchCount = 0;
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

  #lastStatusObf = false;
  #a20Enabled = false;

  constructor(bridge: I8042Bridge, irq: IrqSink, systemControl: { setA20(enabled: boolean): void; requestReset(): void }) {
    this.#bridge = bridge;
    this.#irq = irq;
    this.#systemControl = systemControl;
    // Initial device state should not deliver any IRQ pulses; just synchronize derived state.
    this.#syncSideEffects({ suppressIrqPulses: true });
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
    this.#syncSideEffects({ afterPort60Read: (port & 0xffff) === 0x0060 });
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
    if (this.#bridge.inject_ps2_mouse_motion) {
      this.#bridge.inject_ps2_mouse_motion(dx | 0, dyPs2 | 0, 0);
    } else {
      this.#bridge.inject_mouse_move(dx | 0, dyPs2 | 0);
    }
    this.#syncSideEffects();
  }

  injectMouseButtons(buttons: number): void {
    if (this.#bridge.inject_ps2_mouse_buttons) {
      this.#bridge.inject_ps2_mouse_buttons(buttons & 0xff);
    } else {
      this.#bridge.inject_mouse_buttons(buttons & 0xff);
    }
    this.#syncSideEffects();
  }

  injectMouseWheel(delta: number): void {
    if (this.#bridge.inject_ps2_mouse_motion) {
      this.#bridge.inject_ps2_mouse_motion(0, 0, delta | 0);
    } else {
      this.#bridge.inject_mouse_wheel(delta | 0);
    }
    this.#syncSideEffects();
  }

  save_state(): Uint8Array {
    return this.#bridge.save_state();
  }

  load_state(bytes: Uint8Array): void {
    this.#bridge.load_state(bytes);
    // Snapshot restore should not emit IRQ pulses for any already-buffered output byte.
    this.#syncSideEffects({ suppressIrqPulses: true });
  }

  #syncSideEffects(opts: { afterPort60Read?: boolean; suppressIrqPulses?: boolean } = {}): void {
    this.#syncIrqs(opts);
    this.#syncSystemControl();
  }

  #syncIrqs(opts: { afterPort60Read?: boolean; suppressIrqPulses?: boolean }): void {
    // Prefer explicit IRQ pulses (edge-triggered semantics) when available.
    const drain = (this.#bridge as unknown as { drain_irqs?: unknown }).drain_irqs;
    if (typeof drain === "function") {
      const pulses = (drain as (...args: unknown[]) => unknown).call(this.#bridge);
      const mask = (typeof pulses === "number" ? pulses : 0) & 0xff;
      // bit0: IRQ1, bit1: IRQ12
      if (!opts.suppressIrqPulses) {
        if (mask & 0x01) {
          this.#irq.raiseIrq(1);
          this.#irq.lowerIrq(1);
        }
        if (mask & 0x02) {
          this.#irq.raiseIrq(12);
          this.#irq.lowerIrq(12);
        }
      }
      return;
    }

    // Fallback: older WASM builds only expose `irq_mask()` (level) and do not provide a pulse
    // drain. Deriving pulses by diffing the level is insufficient: the i8042 can refill the
    // output buffer immediately after a port 0x60 read, producing multiple IRQ pulses without the
    // level changing.
    //
    // Approximate pulse semantics by observing output-buffer transitions:
    // - When the output buffer becomes full (OBF 0→1), a byte became available.
    // - After reading port 0x60, if OBF remains set, the buffer refilled and a new byte became
    //   available (and should generate another pulse).
    let status = 0;
    try {
      status = this.#bridge.port_read(0x0064) & 0xff;
    } catch {
      status = 0;
    }
    const obf = (status & 0x01) !== 0;
    if (opts.suppressIrqPulses) {
      this.#lastStatusObf = obf;
      return;
    }

    const shouldPulse = opts.afterPort60Read ? obf : !this.#lastStatusObf && obf;
    if (shouldPulse) {
      const mask = this.#bridge.irq_mask() & 0x03;
      if (mask & 0x01) {
        this.#irq.raiseIrq(1);
        this.#irq.lowerIrq(1);
      }
      if (mask & 0x02) {
        this.#irq.raiseIrq(12);
        this.#irq.lowerIrq(12);
      }
    }
    this.#lastStatusObf = obf;
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
let syntheticUsbConsumerControl: UsbHidPassthroughBridge | null = null;
let syntheticUsbHidAttached = false;
let keyboardUsbOk = false;
let mouseUsbOk = false;
let syntheticUsbKeyboardPendingReport: Uint8Array | null = null;
let syntheticUsbGamepadPendingReport: Uint8Array | null = null;
let syntheticUsbConsumerControlPendingReport: Uint8Array | null = null;
let keyboardInputBackend: InputBackend = "ps2";
const warnedForcedKeyboardBackendUnavailable = new Set<string>();
const pressedKeyboardHidUsages = new Uint8Array(256);
let pressedKeyboardHidUsageCount = 0;
// Track pressed Consumer Control usages (Usage Page 0x0C) so we don't switch keyboard backends
// while a consumer key is held. The IO worker routes some consumer keys through virtio-input and
// others through the synthetic USB consumer-control device; switching the keyboard backend mid-hold
// could otherwise deliver the release to a different backend and leave the previous one "stuck".
//
// The synthetic consumer-control device currently supports usages `0..=0x03FF`, so a 1024-entry
// bitmap is sufficient and avoids per-event allocations.
const pressedConsumerUsages = new Uint8Array(0x0400);
let pressedConsumerUsageCount = 0;
let mouseInputBackend: InputBackend = "ps2";
const warnedForcedMouseBackendUnavailable = new Set<string>();
let mouseButtonsMask = 0;

// End-to-end input latency telemetry (main thread capture -> IO worker processing).
//
// We use wrapping u32 microsecond timestamps (`performance.now() * 1000`) carried in the input
// batch format. To keep overhead minimal, we track a small set of rolling statistics:
// - last batch send->worker latency
// - EWMA of batch send->worker latency
// - max batch send->worker latency since worker start
// - last per-event latency (avg + max across events in the batch)
// - EWMA of per-event average latency
// - max per-event latency since worker start
const INPUT_LATENCY_EWMA_ALPHA = 0.125; // 1/8 smoothing factor
const INPUT_LATENCY_MAX_WINDOW_MS = 1000;
let ioInputLatencyMaxWindowStartMs = 0;
let ioInputBatchSendLatencyEwmaUs = 0;
let ioInputBatchSendLatencyMaxUs = 0;
let ioInputEventLatencyEwmaUs = 0;
let ioInputEventLatencyMaxUs = 0;

let wasmApi: WasmApi | null = null;
let usbPassthroughRuntime: WebUsbPassthroughRuntime | null = null;
let usbPassthroughDebugTimer: number | undefined;
let usbUhciHarnessRuntime: WebUsbUhciHarnessRuntime | null = null;
let usbEhciHarnessRuntime: WebUsbEhciHarnessRuntime | null = null;
let aerogpuDevice: AeroGpuPciDevice | null = null;
let uhciDevice: UhciPciDevice | null = null;
let ehciDevice: EhciPciDevice | null = null;
let xhciDevice: XhciPciDevice | null = null;
let virtioNetDevice: VirtioNetPciDevice | null = null;
let virtioSndDevice: VirtioSndPciDevice | null = null;
type UhciControllerBridge = InstanceType<NonNullable<WasmApi["UhciControllerBridge"]>>;
let uhciControllerBridge: UhciControllerBridge | null = null;
// EHCI is optional and not yet present in all builds; keep as `unknown` so snapshot plumbing can
// preserve controller state when a bridge is wired in.
let ehciControllerBridge: unknown | null = null;
type XhciControllerBridge = InstanceType<NonNullable<WasmApi["XhciControllerBridge"]>>;
let xhciControllerBridge: XhciControllerBridge | null = null;

let e1000Device: E1000PciDevice | null = null;
type E1000Bridge = InstanceType<NonNullable<WasmApi["E1000Bridge"]>>;
let e1000Bridge: E1000Bridge | null = null;

let hdaDevice: HdaPciDevice | null = null;
type HdaControllerBridge = InstanceType<NonNullable<WasmApi["HdaControllerBridge"]>>;
let hdaControllerBridge: HdaControllerBridge | null = null;

type CtorWithLength<T> = { length: number; new (...args: unknown[]): T };
type AnyNewable<T> = { new (...args: unknown[]): T };

type VirtioInputPciDevice = VirtioInputPciDeviceLike;
let virtioInputKeyboard: VirtioInputPciFunction | null = null;
let virtioInputMouse: VirtioInputPciFunction | null = null;
type WebUsbGuestBridge = UsbPassthroughBridgeLike & { set_connected(connected: boolean): void };
let webUsbGuestBridge: WebUsbGuestBridge | null = null;
let webUsbGuestControllerKind: WebUsbGuestControllerKind | null = null;
let webUsbGuestControllerMode: UsbGuestControllerMode = "uhci";
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
const SYNTHETIC_USB_HID_CONSUMER_CONTROL_DEVICE_ID = 0x1000_0004;
const SYNTHETIC_USB_HID_KEYBOARD_PATH: GuestUsbPath = [EXTERNAL_HUB_ROOT_PORT, UHCI_SYNTHETIC_HID_KEYBOARD_HUB_PORT];
const SYNTHETIC_USB_HID_MOUSE_PATH: GuestUsbPath = [EXTERNAL_HUB_ROOT_PORT, UHCI_SYNTHETIC_HID_MOUSE_HUB_PORT];
const SYNTHETIC_USB_HID_GAMEPAD_PATH: GuestUsbPath = [EXTERNAL_HUB_ROOT_PORT, UHCI_SYNTHETIC_HID_GAMEPAD_HUB_PORT];
const SYNTHETIC_USB_HID_CONSUMER_CONTROL_PATH: GuestUsbPath = [
  EXTERNAL_HUB_ROOT_PORT,
  UHCI_SYNTHETIC_HID_CONSUMER_CONTROL_HUB_PORT,
];
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

const MAX_QUEUED_INPUT_BATCH_BYTES = MAX_INPUT_BATCH_RECYCLE_BYTES;
let queuedInputBatchBytes = 0;
const queuedInputBatches: Array<{ buffer: ArrayBuffer; recycle: boolean }> = [];

function postInputBatchRecycle(buffer: ArrayBuffer): void {
  // Input batch recycling is a performance optimization. Avoid recycling extremely large buffers so
  // a malicious or buggy sender cannot force the main thread's recycle pool to retain unbounded
  // memory. The cap matches `MAX_QUEUED_INPUT_BATCH_BYTES` (used to bound snapshot-paused input
  // buffering) so existing tests that intentionally allocate up to that limit remain supported.
  if (!shouldRecycleInputBatchBuffer(buffer, MAX_QUEUED_INPUT_BATCH_BYTES)) return;
  const msg: InputBatchRecycleMessage = { type: "in:input-batch-recycle", buffer };
  try {
    ctx.postMessage(msg, [buffer]);
  } catch {
    try {
      ctx.postMessage(msg);
    } catch {
      // ignore
    }
  }
}

function estimateQueuedSnapshotPausedBytes(msg: unknown): number {
  // We only estimate byte sizes for the high-frequency, byte-bearing message types.
  if (isUsbCompletionMessage(msg)) {
    const completion = msg.completion;
    if (completion.status === "success" && "data" in completion) {
      return completion.data.byteLength >>> 0;
    }
    return 0;
  }
  if (isHidFeatureReportResultMessage(msg)) {
    if (msg.ok && msg.data) return msg.data.byteLength >>> 0;
    return 0;
  }
  if (isHidInputReportMessage(msg)) {
    return msg.data.byteLength >>> 0;
  }
  if (isHidPassthroughFeatureReportResultMessage(msg)) {
    if (msg.ok && msg.data) return msg.data.byteLength >>> 0;
    return 0;
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
      postInputBatchRecycle(entry.buffer);
    }
  }
}
// Intercept async input-related messages while snapshot-paused so WebUSB/WebHID runtimes (and other
// listeners) cannot apply them and mutate guest RAM/device state mid-snapshot.
//
// Use a capturing listener so we run before any runtime-added `addEventListener("message", ...)`
// handlers (which would otherwise process the completion immediately).
ctx.addEventListener(
  "message",
  (ev) => {
    if (machineHostOnlyMode) return;
    if (!snapshotPaused) return;
    const data = (ev as MessageEvent<unknown>).data;
    // Input batches are queued separately so buffers can be recycled after processing.
    if ((data as Partial<InputBatchMessage>)?.type === "in:input-batch") return;

      const shouldQueue =
        isUsbCompletionMessage(data) ||
        isUsbSelectedMessage(data) ||
        isUsbGuestControllerModeMessage(data) ||
        isUsbRingAttachMessage(data) ||
        isUsbRingDetachMessage(data) ||
        isHidProxyMessage(data) ||
        isHidPassthroughAttachHubMessage(data) ||
      isHidPassthroughAttachMessage(data) ||
      isHidPassthroughDetachMessage(data) ||
      isHidPassthroughInputReportMessage(data) ||
      isHidPassthroughFeatureReportResultMessage(data);
    if (!shouldQueue) return;
    ev.stopImmediatePropagation();
    queueSnapshotPausedMessage(data);
  },
  { capture: true },
);
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

function restoreE1000DeviceState(bytes: Uint8Array): void {
  // The E1000 NIC is optional and may not be initialized if virtio-net is present. If the snapshot
  // includes E1000 state but virtio-net is absent, attempt to initialize the NIC before applying
  // state so snapshots remain forwards-compatible across runtime builds.
  if (!e1000Bridge && !virtioNetDevice) {
    maybeInitE1000Device();
  }

  const bridge = e1000Bridge;
  if (!bridge) {
    console.warn("[io.worker] Snapshot contains net.e1000 state but E1000 bridge is unavailable; ignoring blob.");
    return;
  }

  const load =
    (bridge as unknown as { load_state?: unknown }).load_state ??
    (bridge as unknown as { restore_state?: unknown }).restore_state;
  if (typeof load !== "function") {
    console.warn("[io.worker] Snapshot contains net.e1000 state but E1000 bridge has no load_state/restore_state hook; ignoring blob.");
    return;
  }

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
let pendingAudioHdaSnapshotBytes: Uint8Array | null = null;

function resolveAudioHdaSnapshotBridge(): AudioHdaSnapshotBridgeLike | null {
  // Preferred: the snapshot-capable HDA bridge explicitly provided by the audio integration (usually
  // the live IO-worker HDA controller bridge, but only when it supports save/load exports).
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
  if (candidate) {
    if (typeof candidate === "object" || typeof candidate === "function") {
      return candidate as AudioHdaSnapshotBridgeLike;
    }
  }

  // Fall back to the live guest-visible HDA controller bridge (if present). Older WASM builds may
  // not expose snapshot exports on this bridge; callers treat missing hooks as "no snapshot support".
  return hdaControllerBridge as unknown as AudioHdaSnapshotBridgeLike | null;
}

function snapshotAudioHdaDeviceState(): { kind: string; bytes: Uint8Array } | null {
  const bridge = resolveAudioHdaSnapshotBridge();
  if (!bridge) return null;

  const save =
    (bridge as unknown as { save_state?: unknown }).save_state ??
    (bridge as unknown as { snapshot_state?: unknown }).snapshot_state ??
    (bridge as unknown as { saveState?: unknown }).saveState ??
    (bridge as unknown as { snapshotState?: unknown }).snapshotState;
  if (typeof save !== "function") return null;

  const load =
    (bridge as unknown as { load_state?: unknown }).load_state ??
    (bridge as unknown as { restore_state?: unknown }).restore_state ??
    (bridge as unknown as { loadState?: unknown }).loadState ??
    (bridge as unknown as { restoreState?: unknown }).restoreState;
  const canLoad = typeof load === "function";
  if (!canLoad) {
    // If we restored a snapshot previously and the current bridge can't restore HDA state (older
    // WASM build), prefer preserving the cached blob rather than overwriting it with a potentially
    // incompatible "fresh" snapshot from a non-restorable device instance.
    if (snapshotRestoredDeviceBlobs.some((d) => d.kind === VM_SNAPSHOT_DEVICE_AUDIO_HDA_KIND)) {
      return null;
    }
  }
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
  const cachePending = () => {
    // Cache a copy: restored snapshot byte arrays may be backed by transferable buffers (e.g. a
    // transferred ArrayBuffer) or by WASM memory views.
    const copy = new Uint8Array(bytes.byteLength);
    copy.set(bytes);
    pendingAudioHdaSnapshotBytes = copy;
  };
  if (!bridge) {
    // The HDA device bridge may not be initialized yet (WASM init races worker init). Cache the
    // blob and apply it once the bridge becomes available.
    cachePending();
    return;
  }

  const load =
    (bridge as unknown as { load_state?: unknown }).load_state ??
    (bridge as unknown as { restore_state?: unknown }).restore_state ??
    (bridge as unknown as { loadState?: unknown }).loadState ??
    (bridge as unknown as { restoreState?: unknown }).restoreState;
  if (typeof load !== "function") {
    // The bridge exists but doesn't support snapshot restore yet (older WASM build). Keep the blob
    // cached so it can be applied if a compatible bridge becomes available later (e.g. via a global
    // hook).
    cachePending();
    return;
  }
  try {
    load.call(bridge, bytes);
    pendingAudioHdaSnapshotBytes = null;

    // Re-apply the audio output ring buffer (and/or output sample rate) after restore.
    //
    // Why:
    // - The HDA snapshot contains `output_rate_hz` (host output sample rate) for determinism.
    // - The JS-side HdaPciDevice wrapper keeps its own notion of the host output sample rate
    //   and drives its tick clock from it.
    // - `load_state` can update the WASM-side output rate, potentially diverging from the
    //   wrapper's cached sample rate (and from the current host AudioContext rate, if one is
    //   attached). That mismatch can cause incorrect audio pacing (overruns, underruns, perceived
    //   "fast-forward", etc).
    //
    // Calling `setAudioRingBuffer` is idempotent when the same ring buffer is already
    // attached; it plumbs the current host sample rate and keeps the wrapper's tick
    // clock consistent.
    const dev = hdaDevice;
    const ctrl = hdaControllerBridge as unknown as Record<string, unknown> | null;
    // If a host AudioContext is active, it owns the output sample rate.
    // Otherwise (no ring attached), use the restored WASM-side output rate so the wrapper's
    // tick clock stays consistent with the device model.
    let desiredDstSampleRateHz = audioOutDstSampleRate >>> 0;
    if (desiredDstSampleRateHz === 0 && ctrl) {
      let restoredRate = ctrl.output_sample_rate_hz ?? ctrl.outputSampleRateHz;
      if (typeof restoredRate === "function") {
        try {
          restoredRate = (restoredRate as () => unknown).call(hdaControllerBridge);
        } catch {
          restoredRate = undefined;
        }
      }
      if (typeof restoredRate === "number" && Number.isFinite(restoredRate) && restoredRate > 0) {
        desiredDstSampleRateHz = restoredRate >>> 0;
      }
    }

    if (dev && desiredDstSampleRateHz > 0) {
      try {
        dev.setAudioRingBuffer({
          ringBuffer: audioOutRingBuffer,
          capacityFrames: audioOutCapacityFrames,
          channelCount: audioOutChannelCount,
          dstSampleRateHz: desiredDstSampleRateHz,
        });
      } catch (err) {
        console.warn("[io.worker] Failed to reapply audio output settings after HDA snapshot restore", err);
      }
    }

    // Re-apply microphone capture settings after restore for the same reason as output:
    // the snapshot contains `capture_sample_rate_hz` (host mic sample rate) for determinism,
    // but the current host AudioContext may differ. If the guest is consuming the mic ring,
    // mismatched host sample rates can cause resampling drift/pitch changes.
    //
    // `HdaPciDevice.setMicRingBuffer(...)` is idempotent and, when supported by the WASM bridge,
    // will call `attach_mic_ring(ring, sampleRate)` using the wrapper's cached sample rate.
    const hda = hdaDevice;
    if (hda) {
      try {
        if (micSampleRate > 0) hda.setCaptureSampleRateHz(micSampleRate);
        hda.setMicRingBuffer(micRingBuffer);
      } catch (err) {
        console.warn("[io.worker] Failed to reapply microphone settings after HDA snapshot restore", err);
      }
    }
    // Ensure virtio-snd is never a concurrent mic consumer when HDA is present.
    if (hda && virtioSndDevice) {
      try {
        virtioSndDevice.setMicRingBuffer(null);
      } catch {
        // ignore
      }
    }
    // Ensure virtio-snd is never a concurrent audio-ring producer when HDA is present.
    if (hda && virtioSndDevice) {
      try {
        virtioSndDevice.setAudioRingBuffer({
          ringBuffer: null,
          capacityFrames: audioOutCapacityFrames,
          channelCount: audioOutChannelCount,
          dstSampleRateHz: desiredDstSampleRateHz,
        });
      } catch {
        // ignore
      }
    }
  } catch (err) {
    console.warn("[io.worker] HDA audio load_state failed:", err);
    cachePending();
  }
}

let pendingAudioVirtioSndSnapshotBytes: Uint8Array | null = null;

function snapshotAudioVirtioSndDeviceState(): { kind: string; bytes: Uint8Array } | null {
  const dev = virtioSndDevice;
  if (!dev) return null;
  if (!dev.canSaveState()) return null;

  // If we restored virtio-snd state previously but the current runtime cannot restore it (older WASM
  // build), preserve the cached blob rather than overwriting it with a potentially incompatible
  // snapshot.
  if (!dev.canLoadState()) {
    if (snapshotRestoredDeviceBlobs.some((d) => d.kind === VM_SNAPSHOT_DEVICE_AUDIO_VIRTIO_SND_KIND)) {
      return null;
    }
  }

  const bytes = dev.saveState();
  if (bytes instanceof Uint8Array) return { kind: VM_SNAPSHOT_DEVICE_AUDIO_VIRTIO_SND_KIND, bytes };
  return null;
}

function restoreAudioVirtioSndDeviceState(bytes: Uint8Array): void {
  // virtio-snd is optional and may not be initialized yet. Attempt to init before applying state
  // so snapshots remain forwards-compatible across runtime builds.
  if (!virtioSndDevice) {
    maybeInitVirtioSndDevice();
  }

  const cachePending = () => {
    const copy = new Uint8Array(bytes.byteLength);
    copy.set(bytes);
    pendingAudioVirtioSndSnapshotBytes = copy;
  };

  const dev = virtioSndDevice;
  if (!dev) {
    cachePending();
    return;
  }
  if (!dev.canLoadState()) {
    cachePending();
    return;
  }

  const ok = dev.loadState(bytes);
  if (ok) {
    pendingAudioVirtioSndSnapshotBytes = null;

    // Re-apply the current audio output ring configuration after restore.
    //
    // The virtio-snd snapshot includes the host sample rate used for resampling; if the snapshot
    // was produced under a different host AudioContext rate (or if we restore before the UI
    // reattaches the AudioWorklet ring), the restored rate can diverge from the current host.
    //
    // Re-applying the attachment is idempotent and ensures the playback ring + host sample rate
    // are consistent with the current coordinator-provided AudioContext.
    try {
      const shouldAttach = !hdaDevice;
      dev.setAudioRingBuffer({
        ringBuffer: shouldAttach ? audioOutRingBuffer : null,
        capacityFrames: audioOutCapacityFrames,
        channelCount: audioOutChannelCount,
        dstSampleRateHz: audioOutDstSampleRate,
      });
    } catch (err) {
      console.warn("[io.worker] Failed to reapply virtio-snd audio output settings after snapshot restore", err);
    }

    // Re-apply microphone capture ring + sample rate after restore. virtio-snd snapshots also
    // store the host capture rate for determinism, but the live host AudioContext may differ.
    try {
      const shouldAttach = !hdaDevice;
      if (shouldAttach) {
        if (micSampleRate > 0) dev.setCaptureSampleRateHz(micSampleRate);
        dev.setMicRingBuffer(micRingBuffer);
      } else {
        // Ensure we never have two consumers racing the mic ring.
        dev.setMicRingBuffer(null);
      }
    } catch (err) {
      console.warn("[io.worker] Failed to reapply virtio-snd microphone settings after snapshot restore", err);
    }
  } else {
    cachePending();
  }
}

type NetStackSnapshotBridgeLike = {
  save_state?: () => Uint8Array;
  snapshot_state?: () => Uint8Array;
  load_state?: (bytes: Uint8Array) => void;
  restore_state?: (bytes: Uint8Array) => void;
};

// Optional host-side network stack state bridge. This is populated by networking integrations when present.
let netStackBridge: NetStackSnapshotBridgeLike | null = null;

function resolveNetStackSnapshotBridge(): NetStackSnapshotBridgeLike | null {
  if (netStackBridge) return netStackBridge;

  // Allow other runtimes/experiments to attach a network stack bridge via a well-known global.
  // This is intentionally best-effort so snapshots can still be loaded in environments without
  // network-stack support.
  const anyGlobal = globalThis as unknown as Record<string, unknown>;
  const candidate =
    anyGlobal["__aeroNetStackBridge"] ??
    anyGlobal["__aero_net_stack_bridge"] ??
    anyGlobal["__aeroNetStack"] ??
    anyGlobal["__aero_net_stack"] ??
    anyGlobal["__aeroIoNetStack"] ??
    anyGlobal["__aero_io_net_stack"] ??
    anyGlobal["__aero_io_net_stack_bridge"] ??
    null;
  if (!candidate) return null;
  if (typeof candidate !== "object" && typeof candidate !== "function") return null;
  return candidate as NetStackSnapshotBridgeLike;
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

  readonly #webusbDrainActions: ((...args: unknown[]) => unknown) | null;
  readonly #webusbPushCompletion: ((...args: unknown[]) => unknown) | null;
  readonly #webusbDetach: ((...args: unknown[]) => unknown) | null;
  readonly #webusbAttach: ((...args: unknown[]) => unknown) | null;

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

    const uhciAny = opts.uhci as unknown as Record<string, unknown>;
    const drainActions = uhciAny.webusb_drain_actions ?? uhciAny.webusbDrainActions;
    const pushCompletion = uhciAny.webusb_push_completion ?? uhciAny.webusbPushCompletion;
    const detach = uhciAny.webusb_detach ?? uhciAny.webusbDetach;
    const attach = uhciAny.webusb_attach ?? uhciAny.webusbAttach;

    this.#webusbDrainActions = typeof drainActions === "function" ? (drainActions as (...args: unknown[]) => unknown) : null;
    this.#webusbPushCompletion = typeof pushCompletion === "function" ? (pushCompletion as (...args: unknown[]) => unknown) : null;
    this.#webusbDetach = typeof detach === "function" ? (detach as (...args: unknown[]) => unknown) : null;
    this.#webusbAttach = typeof attach === "function" ? (attach as (...args: unknown[]) => unknown) : null;
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
    const drain = this.#webusbDrainActions;
    if (!drain) return null;
    const actions = drain.call(this.#uhci) as UsbHostAction[] | null;
    if (actions == null || actions.length === 0) return null;

    const out: UsbHostAction[] = [];
    for (const action of actions) {
      const brokerId = this.allocBrokerId();
      this.#pendingByBrokerId.set(brokerId, { wasmId: action.id, kind: action.kind });
      out.push(rewriteUsbHostActionId(action, brokerId));
    }
    return out.length === 0 ? null : out;
  }

  push_completion(completion: UsbHostCompletion): void {
    const pushCompletion = this.#webusbPushCompletion;
    if (!pushCompletion) return;
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

    pushCompletion.call(this.#uhci, rewritten);
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
      const detach = this.#webusbDetach;
      if (!detach) throw new Error("UHCI runtime missing webusb_detach export");
      detach.call(this.#uhci);
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
      const attach = this.#webusbAttach;
      if (!attach) throw new Error("UHCI runtime missing webusb_attach export");
      const assigned = attach.call(this.#uhci, this.#rootPort) as number;
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
        const detach = this.#webusbDetach;
        if (!detach) throw new Error("UHCI runtime missing webusb_detach export");
        detach.call(this.#uhci);
        this.#connected = false;
        this.#desiredConnected = null;
        this.#lastError = null;
        this.#onStateChange?.();
        return;
      }

      const attach = this.#webusbAttach;
      if (!attach) throw new Error("UHCI runtime missing webusb_attach export");
      const assigned = attach.call(this.#uhci, this.#rootPort) as number;
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
  const controllerKind = webUsbGuestControllerKind;
  // WebUSB root port numbering:
  // - root port 0 hosts the external hub for WebHID/synthetic HID.
  // - root port 1 is reserved for WebUSB passthrough.
  const rootPort = WEBUSB_GUEST_ROOT_PORT;
  const snapshot: UsbGuestWebUsbSnapshot = {
    available: webUsbGuestBridge !== null,
    attached: webUsbGuestAttached,
    blocked: !usbAvailable,
    controllerKind: controllerKind ?? undefined,
    rootPort,
    lastError: webUsbGuestLastError,
  };

  const prev = lastWebUsbGuestSnapshot;
  if (
    prev &&
    prev.available === snapshot.available &&
    prev.attached === snapshot.attached &&
    prev.blocked === snapshot.blocked &&
    prev.controllerKind === snapshot.controllerKind &&
    prev.rootPort === snapshot.rootPort &&
    prev.lastError === snapshot.lastError
  ) {
    return;
  }
  lastWebUsbGuestSnapshot = snapshot;

  // Routed through the main-thread UsbBroker so the WebUSB broker panel can display guest-visible attachment state.
  ctx.postMessage({ type: "usb.guest.status", snapshot } satisfies UsbGuestWebUsbStatusMessage);
}

function destroyUsbPassthroughRuntime(): void {
  if (usbPassthroughDebugTimer !== undefined) {
    clearInterval(usbPassthroughDebugTimer);
    usbPassthroughDebugTimer = undefined;
  }
  if (usbPassthroughRuntime) {
    usbPassthroughRuntime.destroy();
    usbPassthroughRuntime = null;
  }
}

function disconnectWebUsbBridge(bridge: WebUsbGuestBridge): void {
  try {
    bridge.set_connected(false);
  } catch {
    // ignore
  }
  try {
    bridge.reset();
  } catch {
    // ignore
  }
}

function applyWebUsbGuestControllerMode(mode: UsbGuestControllerMode): void {
  if (webUsbGuestControllerMode === mode) return;
  webUsbGuestControllerMode = mode;

  if (webUsbGuestBridge) {
    disconnectWebUsbBridge(webUsbGuestBridge);
  }
  webUsbGuestBridge = null;
  webUsbGuestControllerKind = null;
  webUsbGuestAttached = false;
  webUsbGuestLastError = null;
  destroyUsbPassthroughRuntime();

  // Force re-init of the WebUSB guest bridge for the new mode.
  maybeInitUhciDevice();
  emitWebUsbGuestStatus();
}

const uhciHidTopology = new UhciHidTopologyManager();
const xhciHidTopology = new XhciHidTopologyManager();
// Source object currently wired into {@link uhciHidTopology}. This is typically the WASM
// `UhciControllerBridge`, but in builds that omit UHCI we may reuse the same topology manager with
// `EhciControllerBridge` since the attachment API is identical (attach_hub/detach_at_path/...).
let uhciHidTopologyBridgeSource: UhciTopologyBridge | null = null;
let xhciHidTopologyBridge: XhciTopologyBridge | null = null;
let xhciHidTopologyBridgeSource: unknown | null = null;
const hidTopologyMux = new IoWorkerHidTopologyMux({
  xhci: xhciHidTopology,
  uhci: uhciHidTopology,
  useXhci: () => xhciHidTopologyBridge !== null,
});

function maybeSendWasmReady(): void {
  if (wasmReadySent) return;
  const init = pendingWasmInit;
  if (!init) return;
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

function isUhciTopologyBridge(value: unknown): value is UhciTopologyBridge {
  if (!value || typeof value !== "object") return false;
  const rec = value as Record<string, unknown>;
  const attachHub = rec.attach_hub ?? rec.attachHub;
  const detachAtPath = rec.detach_at_path ?? rec.detachAtPath;
  const attachWebhid = rec.attach_webhid_device ?? rec.attachWebhidDevice ?? rec.attachWebHidDevice;
  const attachUsbHid = rec.attach_usb_hid_passthrough_device ?? rec.attachUsbHidPassthroughDevice;
  return (
    typeof attachHub === "function" &&
    typeof detachAtPath === "function" &&
    typeof attachWebhid === "function" &&
    typeof attachUsbHid === "function"
  );
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

  const runtimeAny = runtime as unknown as Record<string, unknown>;
  const portRead = runtimeAny.port_read ?? runtimeAny.portRead;
  const portWrite = runtimeAny.port_write ?? runtimeAny.portWrite;
  const stepFrame = runtimeAny.step_frame ?? runtimeAny.stepFrame;
  const tick1ms = runtimeAny.tick_1ms ?? runtimeAny.tick1ms ?? runtimeAny.tick1Ms;
  const irqLevel = runtimeAny.irq_level ?? runtimeAny.irqLevel;
  const free = runtimeAny.free;

  if (typeof portRead !== "function" || typeof portWrite !== "function" || typeof irqLevel !== "function" || typeof free !== "function") {
    console.warn("[io.worker] UHCI runtime missing required port_read/port_write/irq_level/free exports");
    try {
      runtime.free();
    } catch {
      // ignore
    }
    return;
  }

  const bridge: UhciControllerBridgeLike = {
    io_read: (offset, size) => (portRead as (offset: number, size: number) => number).call(runtime, offset >>> 0, size >>> 0) >>> 0,
    io_write: (offset, size, value) =>
      (portWrite as (offset: number, size: number, value: number) => void).call(runtime, offset >>> 0, size >>> 0, value >>> 0),
    ...(typeof stepFrame === "function"
      ? { step_frame: () => (stepFrame as () => void).call(runtime) }
      : {}),
    ...(typeof tick1ms === "function"
      ? { tick_1ms: () => (tick1ms as () => void).call(runtime) }
      : {}),
    irq_asserted: () => Boolean((irqLevel as () => unknown).call(runtime)),
    free: () => {
      try {
        (free as () => void).call(runtime);
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
    uhciHidTopologyBridgeSource = null;
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
  // In the canonical `api.Machine` runtime, the guest PCI topology (including any NIC) is owned by
  // the Machine itself and may attach directly to the shared NET_TX/NET_RX rings. Avoid
  // instantiating a redundant NIC model in the IO worker, which would contend for those rings.
  if (currentConfig?.vmRuntime === "machine") return;
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
    const Ctor = Bridge as unknown as CtorWithLength<E1000Bridge>;
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
  const mode = currentConfig?.virtioInputMode ?? "modern";
  const transportArg: unknown = mode === "transitional" ? true : mode === "legacy" ? "legacy" : undefined;

  // wasm-bindgen's JS glue can enforce constructor arity; try a few common layouts.
  const AnyCtor = Ctor as unknown as AnyNewable<VirtioInputPciDevice>;
  let keyboardDev: VirtioInputPciDevice | null = null;
  let mouseDev: VirtioInputPciDevice | null = null;
  try {
    const construct = (kind: "keyboard" | "mouse"): VirtioInputPciDevice => {
      // Prefer the modern signature with an optional `transport_mode` selector, but fall back to
      // older constructor orderings for compatibility with older wasm-bindgen outputs.
      try {
        return new AnyCtor(base, size, kind, transportArg);
      } catch {
        // ignore and retry
      }
      try {
        return new AnyCtor(base, size, kind);
      } catch {
        // ignore and retry
      }
      try {
        return new AnyCtor(kind, base, size, transportArg);
      } catch {
        // ignore and retry
      }
      return new AnyCtor(kind, base, size);
    };

    keyboardDev = construct("keyboard");
    mouseDev = construct("mouse");
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

  // `keyboardDev`/`mouseDev` are only `null` if construction threw (handled above), but keep an
  // explicit check so TypeScript can narrow for the rest of the function.
  if (!keyboardDev || !mouseDev) return;

  if (mode !== "modern") {
    const probeLegacyIo = (dev: VirtioInputPciDeviceLike): boolean => {
      const devAny = dev as unknown as Record<string, unknown>;
      const read =
        typeof devAny.legacy_io_read === "function"
          ? (devAny.legacy_io_read as (offset: number, size: number) => number)
          : typeof devAny.io_read === "function"
            ? (devAny.io_read as (offset: number, size: number) => number)
            : null;
      const write =
        typeof devAny.legacy_io_write === "function"
          ? (devAny.legacy_io_write as (offset: number, size: number, value: number) => void)
          : typeof devAny.io_write === "function"
            ? (devAny.io_write as (offset: number, size: number, value: number) => void)
            : null;
      if (!read || !write) return false;

      // virtio-pci legacy IO is gated by PCI command bit0 (I/O enable). For the probe we
      // temporarily enable I/O decoding inside the bridge so the read is meaningful.
      const setCmd = typeof devAny.set_pci_command === "function" ? (devAny.set_pci_command as (command: number) => void) : null;
      try {
        if (setCmd) {
          try {
            setCmd.call(dev, 0x0001);
          } catch {
            // ignore
          }
        }
        const probe = (read.call(dev, 0, 4) as number) >>> 0;
        return probe !== 0xffff_ffff;
      } catch {
        return false;
      } finally {
        if (setCmd) {
          try {
            setCmd.call(dev, 0x0000);
          } catch {
            // ignore
          }
        }
      }
    };

    const ok = !!keyboardDev && probeLegacyIo(keyboardDev) && !!mouseDev && probeLegacyIo(mouseDev);
    if (!ok) {
      console.warn(`[io.worker] virtio-input requested mode=${mode}, but legacy I/O is unavailable in this WASM build`);
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
  }

  let keyboardFn: VirtioInputPciFunction | null = null;
  let mouseFn: VirtioInputPciFunction | null = null;
  let keyboardRegistered = false;
  try {
    keyboardFn = new VirtioInputPciFunction({
      kind: "keyboard",
      device: keyboardDev,
      irqSink: mgr.irqSink,
      mode,
    });
    mouseFn = new VirtioInputPciFunction({
      kind: "mouse",
      device: mouseDev,
      irqSink: mgr.irqSink,
      mode,
    });

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

  // Initialize high-speed controllers early so guest WebUSB passthrough can prefer them
  // deterministically when available. UHCI remains initialized for Win7 HID / legacy devices.
  maybeInitEhciDevice();
  maybeInitXhciDevice();

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
        const Ctor = Bridge as unknown as CtorWithLength<UhciControllerBridge>;
        let bridge: UhciControllerBridge;
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
        const maybeTopo: unknown = bridge;
        // Prefer attaching topology devices behind UHCI when the bridge provides the required
        // helpers, but do not clobber an existing EHCI-backed topology bridge if this UHCI build
        // lacks them.
        if (isUhciTopologyBridge(maybeTopo)) {
          uhciHidTopology.setUhciBridge(maybeTopo);
          uhciHidTopologyBridgeSource = maybeTopo;
        } else if (uhciHidTopologyBridgeSource === null) {
          uhciHidTopology.setUhciBridge(null);
          uhciHidTopologyBridgeSource = null;
        }
      } catch (err) {
        console.warn("[io.worker] Failed to initialize UHCI controller bridge", err);
      }
    }
  }

  // Synthetic USB HID devices (keyboard/mouse/gamepad/consumer-control) are attached behind the external hub once
  // a guest-visible UHCI controller exists.
  maybeInitSyntheticUsbHidDevices();

  // WebUSB passthrough is routed based on the selected guest controller mode. Even when the user
  // selects EHCI/xHCI, we keep UHCI initialized for Win7 HID / other legacy paths.
  if (!webUsbGuestBridge) {
    const mode = webUsbGuestControllerMode;

    if (mode === "xhci" || mode === "ehci") {
      const pick =
        mode === "xhci"
          ? chooseWebUsbGuestBridge({ xhciBridge: xhciControllerBridge, ehciBridge: null, uhciBridge: null })
          : chooseWebUsbGuestBridge({ xhciBridge: null, ehciBridge: ehciControllerBridge, uhciBridge: null });
      const label = mode === "xhci" ? "xHCI" : "EHCI";

      if (pick && pick.kind === mode) {
        const ctrl = pick.bridge;
        // The PCI device owns the WASM bridge and calls `free()` during shutdown; wrap with a
        // no-op `free()` so `WebUsbPassthroughRuntime` does not double-free.
        const wrapped: WebUsbGuestBridge = {
          set_connected: (connected) => ctrl.set_connected(connected),
          drain_actions: () => ctrl.drain_actions(),
          push_completion: (completion) => ctrl.push_completion(completion),
          reset: () => ctrl.reset(),
          pending_summary: () => {
            const fn = (ctrl as unknown as { pending_summary?: unknown }).pending_summary;
            if (typeof fn !== "function") return null;
            return fn.call(ctrl) as unknown;
          },
          free: () => {},
        };

        webUsbGuestBridge = wrapped;
        webUsbGuestControllerKind = pick.kind;

        if (!usbPassthroughRuntime) {
          // xHCI/EHCI are high/super-speed controllers: disable the UHCI-only CONFIGURATION→OTHER_SPEED_CONFIGURATION
          // translation in the WebUSB backend so the guest sees the device's real current-speed descriptors.
          //
          // The main-thread UsbBroker routes actions based on the MessagePort they arrive on, so create a
          // dedicated port for the passthrough runtime with translation disabled.
          const port = createUsbBrokerSubportNoOtherSpeedTranslation(ctx);

          usbPassthroughRuntime = new WebUsbPassthroughRuntime({
            bridge: wrapped,
            port,
            pollIntervalMs: 0,
            initiallyBlocked: !usbAvailable,
            // Ring handles received on the main worker channel are not compatible with the dedicated port.
            initialRingAttach: port === ctx ? (usbRingAttach ?? undefined) : undefined,
          });
          usbPassthroughRuntime.start();
          if (IS_DEV) {
            const timer = setInterval(() => {
              console.debug(`[io.worker] ${label} WebUSB pending_summary()`, usbPassthroughRuntime?.pendingSummary());
            }, 1000) as unknown as number;
            (timer as unknown as { unref?: () => void }).unref?.();
            usbPassthroughDebugTimer = timer;
          }
        }

        if (lastUsbSelected) {
          try {
            applyUsbSelectedToWebUsbGuestBridge(pick.kind, wrapped, lastUsbSelected);
            webUsbGuestAttached = lastUsbSelected.ok;
            webUsbGuestLastError = null;
          } catch (err) {
            console.warn(`[io.worker] Failed to apply usb.selected to ${label} WebUSB bridge`, err);
            webUsbGuestAttached = false;
            webUsbGuestLastError = `Failed to apply usb.selected to ${label} WebUSB bridge: ${formatWebUsbGuestError(err)}`;
          }
        } else {
          webUsbGuestAttached = false;
          webUsbGuestLastError = null;
        }

        emitWebUsbGuestStatus();
        return;
      }

      webUsbGuestAttached = false;
      if (usbAvailable) {
        webUsbGuestLastError =
          mode === "xhci"
            ? "XhciControllerBridge unavailable (guest-visible WebUSB passthrough unsupported in this WASM build)."
            : "EhciControllerBridge unavailable (guest-visible WebUSB passthrough unsupported in this WASM build).";
      } else {
        webUsbGuestLastError = null;
      }
      emitWebUsbGuestStatus();
      return;
    }

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
      webUsbGuestControllerKind = "uhci";
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
         if (IS_DEV) {
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
    const pick = chooseWebUsbGuestBridge({ xhciBridge: null, ehciBridge: null, uhciBridge: bridge });
    if (bridge && pick && pick.kind === "uhci") {
      const ctrl = pick.bridge;
      // `UhciPciDevice` owns the WASM bridge and calls `free()` during shutdown; wrap with a
      // no-op `free()` so `WebUsbPassthroughRuntime` does not double-free.
      const wrapped: WebUsbGuestBridge = {
        set_connected: (connected) => ctrl.set_connected(connected),
        drain_actions: () => ctrl.drain_actions(),
        push_completion: (completion) => ctrl.push_completion(completion),
        reset: () => ctrl.reset(),
        // Debug-only; tolerate older WASM builds that might not expose it.
        pending_summary: () => {
          const fn = ctrl.pending_summary;
          if (typeof fn !== "function") return null;
          return fn();
        },
        free: () => {},
      };

      webUsbGuestBridge = wrapped;
      webUsbGuestControllerKind = "uhci";

      if (!usbPassthroughRuntime) {
        usbPassthroughRuntime = new WebUsbPassthroughRuntime({
          bridge: wrapped,
          port: ctx,
          pollIntervalMs: 0,
          initiallyBlocked: !usbAvailable,
          initialRingAttach: usbRingAttach ?? undefined,
       });
       usbPassthroughRuntime.start();
       if (IS_DEV) {
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

function wireXhciHidTopologyBridge(): void {
  const bridge = xhciControllerBridge;
  if (!bridge) {
    xhciHidTopologyBridgeSource = null;
    if (xhciHidTopologyBridge !== null) {
      xhciHidTopologyBridge = null;
      xhciHidTopology.setXhciBridge(null);
    }
    return;
  }

  if (bridge === xhciHidTopologyBridgeSource) return;
  xhciHidTopologyBridgeSource = bridge;
  const shim = createXhciTopologyBridgeShim(bridge);
  xhciHidTopologyBridge = shim;
  xhciHidTopology.setXhciBridge(shim);
}

function maybeInitXhciDevice(): void {
  if (xhciDevice) return;
  const api = wasmApi;
  const mgr = deviceManager;
  if (!api || !mgr) return;
  if (!guestBase) return;

  const res = tryInitXhciDevice({ api, mgr, guestBase, guestSize });
  if (!res) return;

  xhciControllerBridge = res.bridge;
  xhciDevice = res.device;

  // Wire HID topology management if the xHCI bridge exports the required APIs.
  wireXhciHidTopologyBridge();

  // Synthetic USB HID devices (keyboard/mouse/gamepad/consumer-control) may be attached behind xHCI in WASM builds
  // that omit UHCI.
  maybeInitSyntheticUsbHidDevices();
}

function maybeInitEhciDevice(): void {
  if (ehciDevice) return;
  const api = wasmApi;
  const mgr = deviceManager;
  if (!api || !mgr) return;
  if (!guestBase) return;

  // `EhciControllerBridge` is optional and not yet present in all WASM builds, so resolve it via a
  // dynamic lookup (rather than strict typing on `WasmApi`).
  const Bridge = (api as unknown as { EhciControllerBridge?: unknown }).EhciControllerBridge;
  if (typeof Bridge !== "function") return;

  const Ctor = Bridge as unknown as CtorWithLength<EhciControllerBridgeLike>;
  let bridge: EhciControllerBridgeLike | null = null;
  try {
    const base = guestBase >>> 0;
    const size = guestSize >>> 0;

    // wasm-bindgen glue may enforce constructor arity; try a few common layouts:
    // - `new (guestBase, guestSize)`
    // - `new (guestBase)`
    // - `new ()`
    try {
      if (Ctor.length >= 2) {
        bridge = new Ctor(base, size);
      } else if (Ctor.length >= 1) {
        bridge = new Ctor(base);
      } else {
        bridge = new Ctor();
      }
    } catch {
      // Retry with alternate arities to support older/newer bindings.
      try {
        bridge = new Ctor(base, size);
      } catch {
        try {
          bridge = new Ctor(base);
        } catch {
          bridge = new Ctor();
        }
      }
    }

    if (!bridge) throw new Error("EHCI bridge unavailable");
    const dev = new EhciPciDevice({ bridge, irqSink: mgr.irqSink });
    mgr.registerPciDevice(dev);
    mgr.addTickable(dev);

    ehciControllerBridge = bridge;
    ehciDevice = dev;

    // In WASM builds that omit UHCI, reuse the UHCI HID topology manager with EHCI since the
    // exported topology attachment API is identical (attach_hub/detach_at_path/attach_webhid_device/...).
    //
    // This allows WebHID passthrough + synthetic USB HID devices to function in EHCI-only builds.
    if (!uhciRuntime && !uhciControllerBridge && uhciHidTopologyBridgeSource === null) {
      const maybeTopo: unknown = bridge;
      if (isUhciTopologyBridge(maybeTopo)) {
        // EHCI reserves root port 1 for WebUSB passthrough, matching the UHCI topology convention.
        // Keep a shim layer so future EHCI-only topology quirks can be handled without rewriting
        // the UHCI topology manager.
        const shim = createEhciTopologyBridgeShim(maybeTopo);
        uhciHidTopology.setUhciBridge(shim);
        uhciHidTopologyBridgeSource = shim;
      }
    }

    // Synthetic USB HID devices (keyboard/mouse/gamepad/consumer-control) may be attached behind EHCI in WASM builds
    // that omit UHCI/xHCI. Attempt attachment now that the controller exists.
    maybeInitSyntheticUsbHidDevices();
  } catch (err) {
    console.warn("[io.worker] Failed to initialize EHCI controller bridge", err);
    try {
      bridge?.free?.();
    } catch {
      // ignore
    }
  }
}

function maybeInitHdaDevice(): void {
  if (hdaDevice) return;
  const api = wasmApi;
  const mgr = deviceManager;
  if (!api || !mgr) return;
  if (!guestBase) return;

  const Bridge = api.HdaControllerBridge;
  if (!Bridge) return;

  const Ctor = Bridge as unknown as AnyNewable<HdaControllerBridge>;
  let bridge: HdaControllerBridge | null = null;
  try {
    const base = guestBase >>> 0;
    const size = guestSize >>> 0;
    // Best-effort: if the audio output sample rate is already known (e.g. the coordinator
    // attached the AudioWorklet ring before WASM initialization completed), pass it into the
    // HDA bridge constructor so the controller's time base is correct immediately.
    const outputSampleRateHz = audioOutDstSampleRate > 0 ? (audioOutDstSampleRate >>> 0) : undefined;
    // wasm-bindgen's JS glue can enforce constructor arity in some builds; try a few common
    // layouts in descending order of specificity.
    try {
      bridge = new Ctor(base, size, outputSampleRateHz);
    } catch {
      try {
        bridge = new Ctor(base, size);
      } catch {
        bridge = new Ctor(base);
      }
    }
    if (!bridge) throw new Error("HDA bridge unavailable");
    const dev = new HdaPciDevice({ bridge: bridge as HdaControllerBridgeLike, irqSink: mgr.irqSink });
    hdaControllerBridge = bridge;
    // Debug/diagnostics: expose the live HDA bridge in the worker global so DevTools snippets (and
    // snapshot fallback plumbing) can access it without needing internal module bindings.
    //
    // See `docs/testing/audio-windows7.md` for the Win7 audio smoke-test checklist and debugging
    // snippets that use this handle.
    try {
      (globalThis as unknown as { __aeroAudioHdaBridge?: unknown }).__aeroAudioHdaBridge = bridge;
    } catch {
      // ignore
    }
    // Use the live HDA controller as the snapshot bridge *only when* it supports the
    // snapshot exports. Older WASM builds (or experimental runtimes) may not expose
    // `save_state/load_state`; in that case leave `audioHdaBridge` unset so the
    // fallback global hook (`__aeroAudioHdaBridge`, etc.) can still provide snapshot
    // plumbing if present.
    const anyBridge = bridge as unknown as Record<string, unknown>;
    const save = anyBridge.save_state ?? anyBridge.snapshot_state ?? anyBridge.saveState ?? anyBridge.snapshotState;
    const load = anyBridge.load_state ?? anyBridge.restore_state ?? anyBridge.loadState ?? anyBridge.restoreState;
    const canSave = typeof save === "function";
    const canLoad = typeof load === "function";
    audioHdaBridge = canSave && canLoad ? (bridge as AudioHdaSnapshotBridgeLike) : null;
    hdaDevice = dev;
    try {
      const addr = mgr.registerPciDevice(dev, { device: 4, function: 0 });
      // Keep `device.bdf` consistent with what was actually registered for debugging.
      (dev as unknown as { bdf?: { bus: number; device: number; function: number } }).bdf = addr;
    } catch (err) {
      // If the canonical 00:04.0 slot is occupied (or registration otherwise fails),
      // fall back to the PCI bus allocator so the device can still attach.
      const anyDev = dev as unknown as { bdf?: { bus: number; device: number; function: number } };
      const prevBdf = anyDev.bdf;
      try {
        // Temporarily clear `bdf` so registerPciDevice() uses auto allocation instead.
        anyDev.bdf = undefined;
        const addr = mgr.registerPciDevice(dev);
        anyDev.bdf = addr;
      } catch (err2) {
        anyDev.bdf = prevBdf;
        throw err2;
      }
    }
    mgr.addTickable(dev);

    // Apply any existing microphone ring-buffer attachment.
    if (micRingBuffer) {
      // Prefer setting the sample rate before attaching so newer WASM builds can use
      // `attach_mic_ring(ring, sampleRate)` internally (atomic attach+rate).
      if (micSampleRate > 0) dev.setCaptureSampleRateHz(micSampleRate);
      dev.setMicRingBuffer(micRingBuffer);
    }

    // Apply any existing audio output ring-buffer attachment (producer-side).
    // Also plumb host output sample rate even if the ring buffer is currently detached (so the
    // device tick scheduling stays in sync with the AudioContext sample rate).
    if (audioOutRingBuffer || audioOutDstSampleRate > 0) {
      dev.setAudioRingBuffer({
        ringBuffer: audioOutRingBuffer,
        capacityFrames: audioOutCapacityFrames,
        channelCount: audioOutChannelCount,
        dstSampleRateHz: audioOutDstSampleRate,
      });
    }
    // Setting the output rate can implicitly update the capture rate when it was still tracking
    // the previous output rate. Re-apply the current host mic sample rate so capture stays in
    // sync with the live microphone AudioContext.
    if (micSampleRate > 0) {
      try {
        dev.setCaptureSampleRateHz(micSampleRate);
      } catch {
        // ignore
      }
    }

    // If virtio-snd was initialized before HDA (e.g. HDA init failed once and later succeeded),
    // ensure it is detached from the shared audio/mic rings. The AudioWorklet rings are SPSC and
    // must never have multiple producers/consumers.
    if (virtioSndDevice) {
      try {
        virtioSndDevice.setMicRingBuffer(null);
      } catch {
        // ignore
      }
      try {
        virtioSndDevice.setAudioRingBuffer({
          ringBuffer: null,
          capacityFrames: audioOutCapacityFrames,
          channelCount: audioOutChannelCount,
          dstSampleRateHz: audioOutDstSampleRate,
        });
      } catch {
        // ignore
      }
    }

    // Apply any cached snapshot state captured before the HDA bridge was initialized.
    if (pendingAudioHdaSnapshotBytes) {
      const pending = pendingAudioHdaSnapshotBytes;
      pendingAudioHdaSnapshotBytes = null;
      try {
        restoreAudioHdaDeviceState(pending);
      } catch (err) {
        console.warn("[io.worker] Failed to apply pending HDA snapshot state after init", err);
      }
    }
  } catch (err) {
    console.warn("[io.worker] Failed to initialize HDA controller bridge", err);
    try {
      bridge?.free?.();
    } catch {
      // ignore
    }
    hdaControllerBridge = null;
    audioHdaBridge = null;
    hdaDevice = null;
  }
}

function maybeInitSyntheticUsbHidDevices(): void {
  if (syntheticUsbHidAttached) return;
  const api = wasmApi;
  if (!api) return;
  const Bridge = api.UsbHidPassthroughBridge;
  if (!Bridge) return;

  // Ensure a guest-visible USB controller is registered before attaching devices. This is required
  // because PCI hotplug isn't modeled yet.
  if (!uhciDevice && !ehciDevice && !xhciDevice) return;

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
    if (!syntheticUsbConsumerControl) {
      syntheticUsbConsumerControl = new Bridge(
        0x1234,
        0x0004,
        "Aero",
        "Aero USB Consumer Control",
        undefined,
        USB_HID_CONSUMER_CONTROL_REPORT_DESCRIPTOR,
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
  const runtime = uhciRuntime as unknown as Record<string, unknown> | null;
  const attachUsbHidPassthroughDevice = runtime
    ? runtime.attach_usb_hid_passthrough_device ?? runtime.attachUsbHidPassthroughDevice
    : null;
  if (runtime && typeof attachUsbHidPassthroughDevice === "function") {
    try {
      attachUsbHidPassthroughDevice.call(uhciRuntime, SYNTHETIC_USB_HID_KEYBOARD_PATH, syntheticUsbKeyboard);
      attachUsbHidPassthroughDevice.call(uhciRuntime, SYNTHETIC_USB_HID_MOUSE_PATH, syntheticUsbMouse);
      attachUsbHidPassthroughDevice.call(uhciRuntime, SYNTHETIC_USB_HID_GAMEPAD_PATH, syntheticUsbGamepad);
      attachUsbHidPassthroughDevice.call(
        uhciRuntime,
        SYNTHETIC_USB_HID_CONSUMER_CONTROL_PATH,
        syntheticUsbConsumerControl,
      );
      syntheticUsbHidAttached = true;
    } catch (err) {
      console.warn("[io.worker] Failed to attach synthetic USB HID devices to UHCI runtime", err);
    }
    return;
  }

  // Legacy controller bridge path: use the topology manager so hub attachments + reattachments are handled consistently.
  if (uhciHidTopologyBridgeSource !== null) {
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
    uhciHidTopology.attachDevice(
      SYNTHETIC_USB_HID_CONSUMER_CONTROL_DEVICE_ID,
      SYNTHETIC_USB_HID_CONSUMER_CONTROL_PATH,
      "usb-hid-passthrough",
      syntheticUsbConsumerControl,
    );
    syntheticUsbHidAttached = true;
    return;
  }

  // xHCI controller bridge path (WASM builds that omit UHCI/EHCI): attach synthetic devices behind xHCI.
  if (xhciControllerBridge) {
    xhciHidTopology.attachDevice(
      SYNTHETIC_USB_HID_KEYBOARD_DEVICE_ID,
      SYNTHETIC_USB_HID_KEYBOARD_PATH,
      "usb-hid-passthrough",
      syntheticUsbKeyboard,
    );
    xhciHidTopology.attachDevice(
      SYNTHETIC_USB_HID_MOUSE_DEVICE_ID,
      SYNTHETIC_USB_HID_MOUSE_PATH,
      "usb-hid-passthrough",
      syntheticUsbMouse,
    );
    xhciHidTopology.attachDevice(
      SYNTHETIC_USB_HID_GAMEPAD_DEVICE_ID,
      SYNTHETIC_USB_HID_GAMEPAD_PATH,
      "usb-hid-passthrough",
      syntheticUsbGamepad,
    );
    xhciHidTopology.attachDevice(
      SYNTHETIC_USB_HID_CONSUMER_CONTROL_DEVICE_ID,
      SYNTHETIC_USB_HID_CONSUMER_CONTROL_PATH,
      "usb-hid-passthrough",
      syntheticUsbConsumerControl,
    );
    syntheticUsbHidAttached = true;
  }
}

function maybeInitVirtioNetDevice(): void {
  // In the canonical `api.Machine` runtime, the guest PCI topology (including any NIC) is owned by
  // the Machine itself and may attach directly to the shared NET_TX/NET_RX rings. Avoid
  // instantiating a redundant NIC model in the IO worker, which would contend for those rings.
  if (currentConfig?.vmRuntime === "machine") return;
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
    mode: currentConfig?.virtioNetMode,
  });
  if (dev) {
    virtioNetDevice = dev;
  }
}

function teardownGuestNicDevices(): void {
  try {
    virtioNetDevice?.destroy();
  } catch {
    // ignore
  }
  virtioNetDevice = null;

  try {
    e1000Device?.destroy();
  } catch {
    // ignore
  }
  e1000Device = null;
  e1000Bridge = null;
}

function maybeInitVirtioSndDevice(): void {
  if (virtioSndDevice) return;
  const dev = tryInitVirtioSndDevice({
    api: wasmApi,
    mgr: deviceManager,
    guestBase,
    guestSize,
    mode: currentConfig?.virtioSndMode,
  });
  if (!dev) return;
  virtioSndDevice = dev;

  // Apply any existing ring-buffer attachments if HDA audio is unavailable.
  // This keeps audio functional in WASM builds that omit the HDA controller.
  if (!hdaDevice) {
    if (micRingBuffer) {
      try {
        if (micSampleRate > 0) dev.setCaptureSampleRateHz(micSampleRate);
        dev.setMicRingBuffer(micRingBuffer);
      } catch {
        // ignore
      }
    }
    if (audioOutRingBuffer) {
      try {
        dev.setAudioRingBuffer({
          ringBuffer: audioOutRingBuffer,
          capacityFrames: audioOutCapacityFrames,
          channelCount: audioOutChannelCount,
          dstSampleRateHz: audioOutDstSampleRate,
        });
      } catch {
        // ignore
      }
    }
    // Setting the host/output sample rate can implicitly update the capture rate when it was still
    // tracking the previous output rate. Re-apply the current host mic sample rate so capture stays
    // consistent even when mic capture was initialized before audio output.
    if (micSampleRate > 0) {
      try {
        dev.setCaptureSampleRateHz(micSampleRate);
      } catch {
        // ignore
      }
    }
  } else {
    // Ensure the virtio-snd bridge is detached from shared rings when HDA is active.
    try {
      dev.setMicRingBuffer(null);
    } catch {
      // ignore
    }
    try {
      dev.setAudioRingBuffer({
        ringBuffer: null,
        capacityFrames: audioOutCapacityFrames,
        channelCount: audioOutChannelCount,
        dstSampleRateHz: audioOutDstSampleRate,
      });
    } catch {
      // ignore
    }
  }

  // Apply any pending snapshot bytes captured before the virtio-snd device was initialized.
  if (pendingAudioVirtioSndSnapshotBytes) {
    try {
      if (dev.loadState(pendingAudioVirtioSndSnapshotBytes)) {
        pendingAudioVirtioSndSnapshotBytes = null;

        // Re-apply the current host AudioWorklet ring + sample rate plumbing. The snapshot may
        // restore a different host sample rate; keep it consistent with the current AudioContext.
        try {
          const shouldAttach = !hdaDevice;
          dev.setAudioRingBuffer({
            ringBuffer: shouldAttach ? audioOutRingBuffer : null,
            capacityFrames: audioOutCapacityFrames,
            channelCount: audioOutChannelCount,
            dstSampleRateHz: audioOutDstSampleRate,
          });
        } catch {
          // ignore
        }

        // Re-apply microphone capture settings as well: the snapshot may restore a different host
        // capture sample rate, but the current AudioContext may differ.
        try {
          const shouldAttach = !hdaDevice;
          if (shouldAttach) {
            if (micSampleRate > 0) dev.setCaptureSampleRateHz(micSampleRate);
            dev.setMicRingBuffer(micRingBuffer);
          } else {
            dev.setMicRingBuffer(null);
          }
        } catch {
          // ignore
        }
      }
    } catch {
      // ignore
    }
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
let hidOutputRingFallback = 0;
let hidRingDetachSent = false;

const HID_INPUT_RING_MAX_RECORDS_PER_TICK = 256;
const HID_INPUT_RING_MAX_BYTES_PER_TICK = 64 * 1024;

function attachHidRings(msg: HidRingAttachMessage): void {
  // `isHidRingAttachMessage` validates SAB existence + instance checks.
  hidInputRing = new HidReportRing(msg.inputRing);
  hidOutputRing = new HidReportRing(msg.outputRing);
  hidRingDetachSent = false;
}

function detachHidRings(reason: string, options: { notifyBroker?: boolean } = {}): void {
  const hadRings = hidInputRing !== null || hidOutputRing !== null || hidProxyInputRing !== null;
  hidInputRing = null;
  hidOutputRing = null;
  hidProxyInputRing = null;
  hidProxyInputRingForwarded = 0;
  hidProxyInputRingInvalid = 0;
  if (!hadRings) return;

  const shouldNotify = options.notifyBroker !== false;
  if (!shouldNotify) return;
  if (hidRingDetachSent) return;
  hidRingDetachSent = true;
  try {
    ctx.postMessage({ type: "hid.ringDetach", reason } satisfies HidRingDetachMessage);
  } catch {
    // ignore
  }
}

function drainHidInputRing(): void {
  const ring = hidInputRing;
  if (!ring) return;

  try {
    let records = 0;
    let bytes = 0;
    while (records < HID_INPUT_RING_MAX_RECORDS_PER_TICK && bytes < HID_INPUT_RING_MAX_BYTES_PER_TICK) {
      let payloadLen = 0;
      const consumed = ring.consumeNextOrThrow((rec) => {
        payloadLen = rec.payload.byteLength;
        // This ring is only used for `Input` reports, but treat other tags as no-ops so we still
        // advance `head` and can make forward progress if a buggy producer writes them.
        if (rec.reportType !== HidRingReportType.Input) return;
        if (started) Atomics.add(status, StatusIndex.IoHidInputReportCounter, 1);
        try {
          hidGuest.inputReport({
            type: "hid.inputReport",
            deviceId: rec.deviceId,
            reportId: rec.reportId,
            // Ring buffers are backed by SharedArrayBuffer; the WASM bridge accepts Uint8Array views regardless of buffer type.
            data: rec.payload as unknown as Uint8Array<ArrayBuffer>,
          });
        } catch {
          // ignore consumer errors; ring records are best-effort
        }
      });
      if (!consumed) break;
      records += 1;
      bytes += payloadLen;
    }
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    detachHidRings(`HID proxy rings disabled: ${message}`);
  }
}

class CompositeHidGuestBridge implements HidGuestBridge {
  #sinks: HidGuestBridge[];

  constructor(sinks: HidGuestBridge[]) {
    this.#sinks = sinks;
  }

  attach(msg: HidAttachMessage): void {
    for (const sink of this.#sinks) sink.attach(msg);
  }

  detach(msg: HidDetachMessage): void {
    for (const sink of this.#sinks) sink.detach(msg);
  }

  inputReport(msg: HidInputReportMessage): void {
    for (const sink of this.#sinks) sink.inputReport(msg);
  }

  featureReportResult(msg: HidFeatureReportResultMessage): void {
    for (const sink of this.#sinks) sink.featureReportResult?.(msg);
  }

  poll(): void {
    for (const sink of this.#sinks) sink.poll?.();
  }

  destroy(): void {
    for (const sink of this.#sinks) sink.destroy?.();
  }
}

const legacyHidAdapter = new IoWorkerLegacyHidPassthroughAdapter();

let hidAttachResultCapture: { deviceId: number; message: string | null } | null = null;

const hidHostSink: HidHostSink = {
  sendReport: (payload) => {
    const legacyMsg = legacyHidAdapter.sendReport(payload);
    if (legacyMsg) {
      ctx.postMessage(legacyMsg, [legacyMsg.data]);
      return;
    }

    const outRing = hidOutputRing;
    const res = forwardHidSendReportToMainThread(payload, {
      outputRing: outRing,
      postMessage: (msg, transfer) => ctx.postMessage(msg, transfer),
    });
    if (res.path === "postMessage" && res.ringFailed) {
      hidOutputRingFallback += 1;
      const shouldLog = IS_DEV || hidOutputRingFallback <= 3 || (hidOutputRingFallback & 0xff) === 0;
      if (shouldLog) {
        let dropped = 0;
        try {
          dropped = outRing?.dropped() ?? 0;
        } catch {
          dropped = 0;
        }
        console.warn(
          `[io.worker] HID ${payload.reportType} report ring push failed; falling back to postMessage ` +
            `(deviceId=${payload.deviceId} reportId=${payload.reportId} bytes=${payload.data.byteLength} ringDropped=${dropped} fallbackCount=${hidOutputRingFallback})`,
        );
      }
    }
  },
  requestFeatureReport: (payload) => {
    const legacyMsg = legacyHidAdapter.getFeatureReport(payload);
    if (legacyMsg) {
      try {
        ctx.postMessage(legacyMsg);
      } catch {
        // ignore
      }
      return;
    }
    const outputRingTail = (() => {
      const ring = hidOutputRing;
      if (!ring) return undefined;
      try {
        return ring.debugState().tail;
      } catch {
        return undefined;
      }
    })();
    const msg: HidGetFeatureReportMessage = {
      type: "hid.getFeatureReport",
      deviceId: payload.deviceId >>> 0,
      requestId: payload.requestId >>> 0,
      reportId: payload.reportId >>> 0,
      ...(outputRingTail !== undefined ? { outputRingTail } : {}),
    };
    try {
      ctx.postMessage(msg);
    } catch {
      // ignore
    }
  },
  log: (message, deviceId) => {
    const msg: HidLogMessage = { type: "hid.log", message, ...(deviceId !== undefined ? { deviceId } : {}) };
    ctx.postMessage(msg);
  },
  error: (message, deviceId) => {
    const msg: HidErrorMessage = { type: "hid.error", message, ...(deviceId !== undefined ? { deviceId } : {}) };
    if (hidAttachResultCapture && deviceId !== undefined && hidAttachResultCapture.deviceId === deviceId) {
      hidAttachResultCapture.message ??= message;
    }
    ctx.postMessage(msg);
  },
};

const hidGuestInMemory = new InMemoryHidGuestBridge(hidHostSink);
let hidGuest: HidGuestBridge = hidGuestInMemory;
let wasmHidGuest: HidGuestBridge | null = null;

function handleHidFeatureReportResult(msg: HidFeatureReportResultMessage): void {
  const wasm = wasmHidGuest;
  if (!wasm?.completeFeatureReportRequest) {
    if (IS_DEV) {
      console.warn("[hid] Received hid.featureReportResult but no WASM bridge is available", msg);
    }
    return;
  }

  if (msg.ok) {
    const data = msg.data ?? new Uint8Array();
    wasm.completeFeatureReportRequest({ deviceId: msg.deviceId, requestId: msg.requestId, reportId: msg.reportId, data });
  } else {
    if (wasm.failFeatureReportRequest) {
      wasm.failFeatureReportRequest({
        deviceId: msg.deviceId,
        requestId: msg.requestId,
        reportId: msg.reportId,
        error: msg.error ?? "failed to receive feature report",
      });
    } else {
      // Best-effort: older WASM builds may not expose a failure callback. Complete with an empty payload
      // to avoid leaving a guest control transfer hanging indefinitely.
      wasm.completeFeatureReportRequest({ deviceId: msg.deviceId, requestId: msg.requestId, reportId: msg.reportId, data: new Uint8Array() });
    }
  }
}

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
  xhciHidTopology.setHubConfig(msg.guestPath, msg.portCount);
  uhciHidTopology.setHubConfig(msg.guestPath, msg.portCount);
  maybeInitUhciDevice();
  uhciRuntimeHubConfig.setPending(msg.guestPath, msg.portCount);
  uhciRuntimeHubConfig.apply(uhciRuntime, {
    warn: (message, err) => console.warn(`[io.worker] ${message}`, err),
  });
  if (IS_DEV) {
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

  if (IS_DEV) {
    console.info(
      `[hid] attach deviceId=${msg.deviceId} path=${guestPath.join(".")} vid=${hex16(msg.vendorId)} pid=${hex16(msg.productId)}`,
    );
  }

  // Dev-only smoke: issue a best-effort output/feature report request so the
  // worker→main→device round trip is exercised even before the USB stack is wired up.
  if (IS_DEV && !hidPassthroughDebugOutputRequested.has(msg.deviceId)) {
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

  if (IS_DEV) {
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

  if (IS_DEV && (count <= 3 || (count & 0x7f) === 0)) {
    console.debug(
      `[hid] inputReport deviceId=${msg.deviceId} reportId=${msg.reportId} bytes=${msg.data.byteLength} #${count} ${entry.previewHex}`,
    );
  }
}
let currentConfig: AeroConfig | null = null;
let currentConfigVersion = 0;
let vmRuntime: string | null = null;
let machineHostOnlyMode = false;
let machineHostOnlyConsoleLogged = false;
let machineHostOnlyEventLogged = false;
const machineHostOnlyUnavailableLogged = new Set<string>();
const MACHINE_HOST_ONLY_UNAVAILABLE_LOG_LIMIT = 64;

function maybeAnnounceMachineHostOnlyMode(): void {
  if (!machineHostOnlyMode) return;
  const message =
    "vmRuntime=machine; IO worker running in machine host-only mode (skipping all guest device models + ioIpc server; devices/disks owned by CPU worker)";
  if (!machineHostOnlyConsoleLogged) {
    machineHostOnlyConsoleLogged = true;
    console.info(`[io.worker] ${message}`);
  }
  // Event-ring logging requires shared memory init; defer until `eventRing` exists.
  if (!machineHostOnlyEventLogged && eventRing) {
    machineHostOnlyEventLogged = true;
    pushEvent({ kind: "log", level: "info", message });
  }
}

function machineHostOnlyUnavailable(feature: string): void {
  if (!machineHostOnlyMode) return;
  if (machineHostOnlyUnavailableLogged.has(feature)) return;
  if (machineHostOnlyUnavailableLogged.size >= MACHINE_HOST_ONLY_UNAVAILABLE_LOG_LIMIT) return;
  machineHostOnlyUnavailableLogged.add(feature);
  const message = `${feature} unavailable in vmRuntime=machine host-only mode`;
  console.warn(`[io.worker] ${message}`);
  pushEvent({ kind: "log", level: "warn", message });
}

function machineHostOnlyMessageLabel(data: unknown): string {
  if (!data || typeof data !== "object") return "message";
  const rec = data as { type?: unknown; kind?: unknown };
  if (typeof rec.type === "string") return rec.type;
  if (typeof rec.kind === "string") return rec.kind;
  return "message";
}

function stopIoIpcServerForMachineHostOnlyMode(): void {
  // Best-effort stop of the guest IO RPC loop. This is used when the worker learns it is running
  // under `vmRuntime=machine` *after* it already started guest device models.
  //
  // IMPORTANT: The IO worker must remain alive in this mode (it still participates in the
  // coordinator protocol and must ACK snapshot pause/resume), so do not call `shutdown()`.
  const abort = ioServerAbort;
  if (!abort) return;
  if (ioServerExitMode === "host-only") return;
  ioServerExitMode = "host-only";
  try {
    abort.abort();
  } catch {
    // ignore
  }
}

function teardownGuestStateForMachineHostOnlyMode(): void {
  // Stop any per-tick guest loops first so we don't contend for shared rings (NET/HID/disk) with
  // the canonical Machine runtime.
  stopIoIpcServerForMachineHostOnlyMode();

  // Tear down any guest-side resources that may have been initialized before we learned the VM
  // runtime is "machine". This is best-effort: the coordinator owns the authoritative guest state.
  try {
    if (audioOutTelemetryTimer !== undefined) {
      clearInterval(audioOutTelemetryTimer);
      audioOutTelemetryTimer = undefined;
    }
  } catch {
    // ignore
  }
  try {
    if (usbPassthroughDebugTimer !== undefined) {
      clearInterval(usbPassthroughDebugTimer);
      usbPassthroughDebugTimer = undefined;
    }
  } catch {
    // ignore
  }

  // Detach HID rings so we stop draining SharedArrayBuffer-backed report queues.
  try {
    detachHidRings("vmRuntime=machine host-only mode", { notifyBroker: false });
  } catch {
    // ignore
  }

  // Disk: release OPFS sync handles by terminating the runtime disk worker. The Machine runtime
  // opens its own exclusive `FileSystemSyncAccessHandle`s.
  activeDisk = null;
  cdDisk = null;
  pendingBootDisks = null;
  try {
    diskClient?.close();
  } catch {
    // ignore
  }
  diskClient = null;
  diskIoChain = Promise.resolve();

  try {
    usbHid?.free();
  } catch {
    // ignore
  }
  usbHid = null;

  try {
    syntheticUsbKeyboard?.free();
  } catch {
    // ignore
  }
  syntheticUsbKeyboard = null;
  try {
    syntheticUsbMouse?.free();
  } catch {
    // ignore
  }
  syntheticUsbMouse = null;
  try {
    syntheticUsbGamepad?.free();
  } catch {
    // ignore
  }
  syntheticUsbGamepad = null;
  try {
    syntheticUsbConsumerControl?.free();
  } catch {
    // ignore
  }
  syntheticUsbConsumerControl = null;
  syntheticUsbHidAttached = false;
  syntheticUsbKeyboardPendingReport = null;
  syntheticUsbGamepadPendingReport = null;
  syntheticUsbConsumerControlPendingReport = null;

  // Reset input backend selection state so host-only mode doesn't attempt to inject into guest devices.
  keyboardInputBackend = "ps2";
  pressedKeyboardHidUsages.fill(0);
  pressedKeyboardHidUsageCount = 0;
  pressedConsumerUsages.fill(0);
  pressedConsumerUsageCount = 0;
  mouseInputBackend = "ps2";
  mouseButtonsMask = 0;

  webUsbGuestBridge = null;
  webUsbGuestControllerKind = null;
  uhciRuntimeWebUsbBridge = null;

  try {
    usbPassthroughRuntime?.destroy();
  } catch {
    // ignore
  }
  usbPassthroughRuntime = null;

  try {
    usbUhciHarnessRuntime?.destroy();
  } catch {
    // ignore
  }
  usbUhciHarnessRuntime = null;
  try {
    usbEhciHarnessRuntime?.destroy();
  } catch {
    // ignore
  }
  usbEhciHarnessRuntime = null;

  // Guest USB controller models.
  try {
    uhciDevice?.destroy();
  } catch {
    // ignore
  }
  uhciDevice = null;
  try {
    ehciDevice?.destroy();
  } catch {
    // ignore
  }
  ehciDevice = null;
  try {
    xhciDevice?.destroy();
  } catch {
    // ignore
  }
  xhciDevice = null;
  uhciRuntime = null;
  uhciControllerBridge = null;
  ehciControllerBridge = null;
  xhciControllerBridge = null;

  // Guest NIC models.
  teardownGuestNicDevices();

  // Guest input models.
  try {
    virtioInputKeyboard?.destroy();
  } catch {
    // ignore
  }
  virtioInputKeyboard = null;
  try {
    virtioInputMouse?.destroy();
  } catch {
    // ignore
  }
  virtioInputMouse = null;

  // Reset HID topology routing.
  uhciHidTopology.setUhciBridge(null);
  uhciHidTopologyBridgeSource = null;
  xhciHidTopologyBridge = null;
  xhciHidTopologyBridgeSource = null;
  xhciHidTopology.setXhciBridge(null);
  const prevHidGuest = hidGuest;
  try {
    prevHidGuest.destroy?.();
  } catch {
    // ignore
  }
  uhciRuntimeHidGuest = null;
  wasmHidGuest = null;
  hidGuest = hidGuestInMemory;

  // Guest audio devices.
  try {
    hdaDevice?.destroy();
  } catch {
    // ignore
  }
  hdaDevice = null;
  hdaControllerBridge = null;
  audioHdaBridge = null;
  pendingAudioHdaSnapshotBytes = null;

  try {
    virtioSndDevice?.destroy();
  } catch {
    // ignore
  }
  virtioSndDevice = null;

  // Guest cursor forwarding.
  aerogpuDevice = null;

  // WebUSB demo runtime (dev-only harness).
  try {
    usbDemoApi?.free();
  } catch {
    // ignore
  }
  usbDemoApi = null;
  usbDemo = null;
  lastUsbSelected = null;

  // Clear any buffered async IO events so we stop touching the IO event ring.
  pendingIoEvents.length = 0;

  // Finally, clear buses/rings that are guest-owned in machine mode.
  deviceManager = null;
  netTxRing = null;
  netRxRing = null;
  hidInRing = null;
  ioCmdRing = null;
  ioEvtRing = null;
  ioIpcSab = null;
  i8042Ts = null;
  try {
    i8042Wasm?.free();
  } catch {
    // ignore
  }
  i8042Wasm = null;
}

function setVmRuntimeFromConfigUpdate(update: ConfigUpdateMessage): void {
  // `vmRuntime` is supplied by the coordinator/runtime layer (not part of AeroConfig).
  // Support both shapes:
  // - `{ kind: "config.update", vmRuntime: "machine", ... }`
  // - `{ kind: "config.update", config: { vmRuntime: "machine", ... }, ... }` (legacy/compat)
  const anyUpdate = update as unknown as { vmRuntime?: unknown; config?: unknown };
  const next =
    typeof anyUpdate.vmRuntime === "string"
      ? anyUpdate.vmRuntime
      : typeof (update.config as unknown as { vmRuntime?: unknown })?.vmRuntime === "string"
        ? ((update.config as unknown as { vmRuntime?: unknown }).vmRuntime as string)
        : null;
  if (!next || next === vmRuntime) return;
  vmRuntime = next;

  const nextHostOnly = vmRuntime === "machine";
  if (nextHostOnly && !machineHostOnlyMode) {
    machineHostOnlyMode = true;
    // `setBootDisks` opens OPFS sync handles via runtime_disk_worker. In machine runtime the CPU
    // worker owns disk attachment and must be able to open the same OPFS file (sync handles are
    // exclusive), so unblock init immediately and ignore future disk opens.
    if (bootDisksInitResolve) {
      bootDisksInitResolve();
      bootDisksInitResolve = null;
    }
    pendingBootDisks = null;
    maybeAnnounceMachineHostOnlyMode();
    teardownGuestStateForMachineHostOnlyMode();
  }
}

function maybeInitWasmHidGuestBridge(): void {
  const api = wasmApi;
  if (!api) return;

  // Ensure guest-visible USB controllers are registered before wiring up WebHID devices. If we
  // initialize the bridge before the UHCI controller exists, devices would never be visible to the
  // guest OS (PCI hotplug isn't modeled yet).
  maybeInitUhciDevice();
  maybeInitEhciDevice();
  maybeInitXhciDevice();
  wireXhciHidTopologyBridge();
  const hasXhciTopology = xhciHidTopologyBridge !== null;
  const hasUhciTopology = uhciHidTopologyBridgeSource !== null;
  if (wasmHidGuest) {
    maybeSendWasmReady();
    return;
  }
  if (!hasXhciTopology && !uhciRuntime && !hasUhciTopology) {
    // No active topology backend. Wait for controllers to initialize when possible.
    if (api.XhciControllerBridge && !xhciControllerBridge) return;
    if (api.UhciControllerBridge && !uhciControllerBridge) return;
    const hasEhciCtor = typeof (api as unknown as { EhciControllerBridge?: unknown }).EhciControllerBridge === "function";
    if (hasEhciCtor && !ehciControllerBridge) return;
    return;
  }

  try {
    // Prefer the UHCI runtime WebHID backend when it is available. Guest USB paths (`GuestUsbPath`)
    // are defined in terms of the UHCI root hub ports, and the runtime implements Aero's external
    // hub + synthetic HID contract on top of that topology.
    //
    // xHCI topology management is primarily used in WASM builds that omit UHCI; avoid routing
    // WebHID passthrough to xHCI in runtime builds so browser E2E tests (and legacy guest stacks)
    // can deterministically enumerate the devices via the UHCI controller model.
    if (uhciRuntime) {
      uhciRuntimeHidGuest = new WasmUhciHidGuestBridge({ uhci: uhciRuntime, host: hidHostSink });
      wasmHidGuest = uhciRuntimeHidGuest;
    } else if (hasXhciTopology) {
      wasmHidGuest = new WasmHidGuestBridge(api, hidHostSink, hidTopologyMux);
    } else {
      wasmHidGuest = new WasmHidGuestBridge(api, hidHostSink, hidTopologyMux);
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
  if (IS_DEV) {
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
let ioServerExitMode: "shutdown" | "host-only" | null = null;
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

type HdaMicCaptureTestMessage = {
  type: "hda.micCaptureTest";
  requestId: number;
};

type HdaCodecDebugStateRequestMessage = {
  type: "hda.codecDebugState";
  requestId: number;
};

type HdaCodecDebugStateResultMessage =
  | {
      type: "hda.codecDebugStateResult";
      requestId: number;
      ok: true;
      state: unknown;
    }
  | {
      type: "hda.codecDebugStateResult";
      requestId: number;
      ok: false;
      error: string;
    };

type HdaSnapshotStateRequestMessage = {
  type: "hda.snapshotState";
  requestId: number;
};

type HdaSnapshotStateResultMessage =
  | {
      type: "hda.snapshotStateResult";
      requestId: number;
      ok: true;
      bytes: Uint8Array;
    }
  | {
      type: "hda.snapshotStateResult";
      requestId: number;
      ok: false;
      error: string;
    };

type HdaTickStatsRequestMessage = {
  type: "hda.tickStats";
  requestId: number;
};

type HdaTickStatsResultMessage =
  | {
      type: "hda.tickStatsResult";
      requestId: number;
      ok: true;
      stats: { tickClampEvents: number; tickClampedFramesTotal: number; tickDroppedFramesTotal: number };
    }
  | {
      type: "hda.tickStatsResult";
      requestId: number;
      ok: false;
      error: string;
    };

type VirtioSndSnapshotStateRequestMessage = {
  type: "virtioSnd.snapshotState";
  requestId: number;
};

type VirtioSndSnapshotStateResultMessage =
  | {
      type: "virtioSnd.snapshotStateResult";
      requestId: number;
      ok: true;
      bytes: Uint8Array;
    }
  | {
      type: "virtioSnd.snapshotStateResult";
      requestId: number;
      ok: false;
      error: string;
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
let audioOutTelemetryTimer: number | undefined = undefined;

const AUDIO_OUT_TELEMETRY_INTERVAL_MS = 50;

// Low-rate tick-clamp telemetry for HDA (worker stall observability).
const HDA_TICK_TELEMETRY_INTERVAL_MS = 250;
let hdaTickTelemetryNextMs = 0;

function startAudioOutTelemetryTimer(): void {
  if (audioOutTelemetryTimer !== undefined) return;
  const timer = setInterval(() => {
    // Match the IO tick loop behaviour: when snapshot-paused, freeze device-side
    // state and avoid writing to shared status.
    if (snapshotPaused) return;
    const now = typeof performance?.now === "function" ? performance.now() : Date.now();
    maybePublishAudioOutTelemetry(now);
  }, AUDIO_OUT_TELEMETRY_INTERVAL_MS) as unknown as number;
  (timer as unknown as { unref?: () => void }).unref?.();
  audioOutTelemetryTimer = timer;

  // Publish once immediately so callers that attach the ring after the IO IPC
  // server has started don't need to wait for the first interval tick.
  const now = typeof performance?.now === "function" ? performance.now() : Date.now();
  maybePublishAudioOutTelemetry(now);
}

function maybePublishHdaTickTelemetry(nowMs: number): void {
  // `perf.counter()` is a no-op when tracing is disabled, but avoid the
  // `HdaPciDevice.getTickStats()` allocation unless it's actually needed.
  if (!perf.traceEnabled) return;
  if (!Number.isFinite(nowMs)) return;

  const hda = hdaDevice;
  if (!hda) return;
  if (hdaTickTelemetryNextMs !== 0 && nowMs < hdaTickTelemetryNextMs) return;
  hdaTickTelemetryNextMs = nowMs + HDA_TICK_TELEMETRY_INTERVAL_MS;

  const { tickClampEvents, tickClampedFramesTotal, tickDroppedFramesTotal } = hda.getTickStats();
  perf.counter("audio.hda.tickClampEvents", tickClampEvents);
  perf.counter("audio.hda.tickClampedFrames", tickClampedFramesTotal);
  perf.counter("audio.hda.tickDroppedFrames", tickDroppedFramesTotal);
}

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
  const parsePositiveSafeU32 = (value: unknown): number => {
    if (typeof value !== "number" || !Number.isSafeInteger(value) || value <= 0 || value > 0xffff_ffff) return 0;
    return value >>> 0;
  };

  if (ringBuffer !== null) {
    const Sab = globalThis.SharedArrayBuffer;
    if (typeof Sab === "undefined") {
      console.warn("[io.worker] SharedArrayBuffer is unavailable; dropping mic ring attachment.");
      ringBuffer = null;
    } else if (!(ringBuffer instanceof Sab)) {
      console.warn("[io.worker] setMicrophoneRingBuffer expects a SharedArrayBuffer or null; dropping attachment.");
      ringBuffer = null;
    } else {
      // Best-effort validate ring buffer layout so we don't propagate obviously invalid SABs into
      // the device model (which would otherwise throw during WASM `MicBridge` construction).
      try {
        const byteLen = ringBuffer.byteLength;
        if (byteLen < MIC_HEADER_BYTES) {
          throw new Error(`mic ring buffer too small (${byteLen} bytes)`);
        }

        const payloadBytes = byteLen - MIC_HEADER_BYTES;
        if (payloadBytes <= 0 || payloadBytes % Float32Array.BYTES_PER_ELEMENT !== 0) {
          throw new Error("mic ring buffer payload is not 4-byte aligned");
        }

        const payloadSamples = payloadBytes / Float32Array.BYTES_PER_ELEMENT;
        const header = new Uint32Array(ringBuffer, 0, MIC_HEADER_U32_LEN);
        const capFromHeader = Atomics.load(header, MIC_CAPACITY_SAMPLES_INDEX) >>> 0;
        const capacitySamples = capFromHeader !== 0 ? capFromHeader : payloadSamples;
        if (capFromHeader !== 0 && capFromHeader !== payloadSamples) {
          throw new Error("mic ring buffer capacity does not match SharedArrayBuffer size");
        }
        const MAX_CAPACITY_SAMPLES = 1_048_576; // keep in sync with mic_ring.js + Rust MicBridge cap
        if (!Number.isFinite(capacitySamples) || capacitySamples <= 0 || capacitySamples > MAX_CAPACITY_SAMPLES) {
          throw new Error(`mic ring buffer capacity out of range: ${capacitySamples}`);
        }
      } catch (err) {
        console.warn("[io.worker] mic ring buffer validation failed; dropping attachment:", err);
        ringBuffer = null;
      }
    }
  }

  micRingBuffer = ringBuffer;
  micSampleRate = parsePositiveSafeU32(sampleRate);

  // Microphone ring buffers are SPSC (single-consumer). Prefer the legacy HDA
  // model when available; fall back to virtio-snd in builds that omit HDA.
  const hda = hdaDevice;
  const snd = virtioSndDevice;
  if (hda) {
    // Prefer setting the sample rate before attaching so HdaPciDevice can use
    // `attach_mic_ring(ring, sampleRate)` in one step when available (avoids an
    // extra attach/detach cycle when the previous rate was 0).
    if (ringBuffer && micSampleRate > 0) {
      try {
        hda.setCaptureSampleRateHz(micSampleRate);
      } catch {
        // ignore
      }
    }
    try {
      hda.setMicRingBuffer(ringBuffer);
    } catch {
      // ignore
    }
    // Ensure we never have two consumers racing the mic ring.
    if (snd) {
      try {
        snd.setMicRingBuffer(null);
      } catch {
        // ignore
      }
    }
  } else if (snd) {
    if (ringBuffer && micSampleRate > 0) {
      try {
        snd.setCaptureSampleRateHz(micSampleRate);
      } catch {
        // ignore
      }
    }
    try {
      snd.setMicRingBuffer(ringBuffer);
    } catch {
      // ignore
    }
  }
}

type HdaMicCaptureTestResultMessage =
  | { type: "hda.micCaptureTest.result"; requestId: number; ok: true; pcm: ArrayBuffer; lpibBefore: number; lpibAfter: number }
  | { type: "hda.micCaptureTest.result"; requestId: number; ok: false; error: string };

function runHdaMicCaptureTest(requestId: number): void {
  maybeInitHdaDevice();

  const bridge = hdaControllerBridge as unknown as {
    mmio_read?: unknown;
    mmio_write?: unknown;
    step_frames?: unknown;
  } | null;
  if (
    !bridge ||
    typeof bridge.mmio_read !== "function" ||
    typeof bridge.mmio_write !== "function" ||
    typeof bridge.step_frames !== "function"
  ) {
    ctx.postMessage({
      type: "hda.micCaptureTest.result",
      requestId,
      ok: false,
      error: "HDA controller bridge is unavailable.",
    } satisfies HdaMicCaptureTestResultMessage);
    return;
  }
  if (!micRingBuffer) {
    ctx.postMessage({
      type: "hda.micCaptureTest.result",
      requestId,
      ok: false,
      error: "Microphone ring buffer is not attached.",
    } satisfies HdaMicCaptureTestResultMessage);
    return;
  }

  // The harness drives the WASM HDA model directly via `mmio_write` + `step_frames`, bypassing the
  // guest's PCI config-space command register writes. Newer WASM builds gate all DMA (CORB/RIRB,
  // PCM, etc.) on PCI Bus Master Enable (command bit 2), so ensure that bit is set while the test
  // runs. Do this via the JS PCI config-port model (0xCF8/0xCFC) so the device wrapper's
  // `onPciCommandWrite` hook (and the WASM bridge's `set_pci_command` mirror) stay coherent.
  //
  // Restore both the config-address register and the original command value so the harness does not
  // perturb any concurrently running guest code.
  const mgr = deviceManager;
  const hdaBdf = (hdaDevice as unknown as { bdf?: { bus: number; device: number; function: number } } | null)?.bdf ?? {
    bus: 0,
    device: 4,
    function: 0,
  };
  const cfgAddrHdaCommand =
    (0x8000_0000 |
      ((hdaBdf.bus & 0xff) << 16) |
      ((hdaBdf.device & 0x1f) << 11) |
      ((hdaBdf.function & 0x7) << 8) |
      0x04) >>> 0;
  let restorePciAddrReg: number | null = null;
  let restorePciCommand: number | null = null;

  try {
    if (mgr) {
      try {
        restorePciAddrReg = mgr.portRead(0x0cf8, 4) >>> 0;
        mgr.portWrite(0x0cf8, 4, cfgAddrHdaCommand);
        restorePciCommand = mgr.portRead(0x0cfc, 2) & 0xffff;

        const withBusMaster = (restorePciCommand | (1 << 2)) & 0xffff;
        if (withBusMaster !== restorePciCommand) {
          // Only touch the command word (2 bytes). Avoid writing the full dword, which would risk
          // clearing RW1C PCI status bits.
          mgr.portWrite(0x0cfc, 2, withBusMaster);
        }
      } finally {
        if (restorePciAddrReg !== null) {
          mgr.portWrite(0x0cf8, 4, restorePciAddrReg);
        }
      }
    }

    // Guest memory layout.
    //
    // IMPORTANT: Keep this region disjoint from the CPU worker's always-on guest-memory
    // framebuffer demos:
    // - Shared framebuffer embed offset starts at `CPU_WORKER_DEMO_FRAMEBUFFER_OFFSET_BYTES` (0x20_0000).
    // - Demo framebuffer scratch uses `DEMO_FB_OFFSET` (0x50_0000) for up to ~3MiB.
    //
    // The CPU worker writes those regions continuously, so overlapping addresses will corrupt
    // CORB/RIRB/BDL/PCM state and make this test flaky.
    const corbBase = 0x0140_0000;
    const rirbBase = 0x0140_1000;
    const bdlBase = 0x0141_0000;
    const pcmBase = 0x0142_0000;
    const pcmBytes = 4096;

    const view = new DataView(guestU8.buffer, guestU8.byteOffset, guestU8.byteLength);

    const ensureRange = (guestOffset: number, len: number) => {
      const off = guestOffset >>> 0;
      const end = off + (len >>> 0);
      if (end > guestU8.byteLength) {
        throw new Error(
          `HDA test guest range out of bounds: off=0x${off.toString(16)} len=0x${len.toString(16)} guestBytes=0x${guestU8.byteLength.toString(16)}`,
        );
      }
    };

    const CORB_ENTRIES = 256;
    const RIRB_ENTRIES = 256;
    const CORB_BYTES = CORB_ENTRIES * 4;
    const RIRB_BYTES = RIRB_ENTRIES * 8;

    // Ensure these fixed-offset harness buffers do not overlap the always-on shared framebuffer demo
    // region embedded in guest RAM.
    assertNoGuestOverlapWithSharedFramebuffer(corbBase, CORB_BYTES, "HDA test CORB");
    assertNoGuestOverlapWithSharedFramebuffer(rirbBase, RIRB_BYTES, "HDA test RIRB");
    assertNoGuestOverlapWithSharedFramebuffer(bdlBase, 16, "HDA test BDL");
    assertNoGuestOverlapWithSharedFramebuffer(pcmBase, pcmBytes, "HDA test PCM");

    const writeU32 = (guestOffset: number, value: number) => {
      ensureRange(guestOffset, 4);
      view.setUint32(guestOffset >>> 0, value >>> 0, true);
    };

    const writeU64 = (guestOffset: number, value: number) => {
      ensureRange(guestOffset, 8);
      writeU32(guestOffset, value >>> 0);
      writeU32(guestOffset + 4, 0);
    };

    // Clear the PCM target buffer so the test can assert on a non-zero write.
    ensureRange(pcmBase, pcmBytes);
    guestU8.fill(0, pcmBase, pcmBase + pcmBytes);

    // One BDL entry pointing at the PCM buffer.
    ensureRange(bdlBase, 16);
    writeU64(bdlBase + 0, pcmBase);
    writeU32(bdlBase + 8, pcmBytes);
    writeU32(bdlBase + 12, 1); // IOC

    // Program CORB verbs to configure the input converter (NID4) for stream 2, ch0 and a basic format.
    // Also enable the mic pin widget (NID5); the codec defaults to gating capture to silence
    // until the pin is enabled.
    const cmd = (cad: number, nid: number, verb20: number) => ((cad << 28) | (nid << 20) | (verb20 & 0x000f_ffff)) >>> 0;
    const setStreamCh = (0x706 << 8) | 0x20; // stream=2, channel=0
    const setFmt = (0x200 << 8) | 0x10; // 48kHz, 16-bit, mono (matches fmt raw below)
    const setMicPinCtl = (0x707 << 8) | 0x20; // PinWidgetControl: IN_EN

    ensureRange(corbBase, CORB_BYTES);
    ensureRange(rirbBase, RIRB_BYTES);
    writeU32(corbBase + 0, cmd(0, 4, setStreamCh));
    writeU32(corbBase + 4, cmd(0, 4, setFmt));
    writeU32(corbBase + 8, cmd(0, 5, setMicPinCtl));

    // wasm-bindgen class methods rely on `this.__wbg_ptr`. If we detach methods from the
    // object (e.g. `const f = bridge.mmio_write; f(...)`) then `this` becomes `undefined`
    // and the generated glue crashes trying to read `__wbg_ptr`. Wrap with `.call()` so
    // we always invoke the methods with the correct receiver.
    const mmioWrite = (offset: number, size: number, value: number): void => {
      (bridge.mmio_write as (offset: number, size: number, value: number) => void).call(bridge, offset, size, value);
    };
    const mmioRead = (offset: number, size: number): number => {
      return (bridge.mmio_read as (offset: number, size: number) => number).call(bridge, offset, size) >>> 0;
    };
    const stepFrames = (frames: number): void => {
      (bridge.step_frames as (frames: number) => void).call(bridge, frames);
    };

    // Reset the controller to a known state, then bring it out of reset.
    //
    // This keeps repeated harness calls deterministic (e.g. avoids capture resampler
    // state carrying over between runs, which would otherwise leak non-zero samples
    // into the "mic ring empty -> silence" case).
    mmioWrite(0x08, 4, 0x0); // GCTL.CRST=0 (enter reset)
    mmioWrite(0x08, 4, 0x1); // GCTL.CRST=1 (leave reset)

    // Use 256-entry CORB/RIRB rings so we can enqueue multiple verbs without dealing with
    // 2-entry pointer masking semantics (CORBWP becomes 1-bit wide when CORBSIZE=2).
    mmioWrite(0x4e, 1, 0x2); // CORBSIZE: 256 entries
    mmioWrite(0x5e, 1, 0x2); // RIRBSIZE: 256 entries
    mmioWrite(0x40, 4, corbBase); // CORBLBASE
    mmioWrite(0x44, 4, 0); // CORBUBASE
    mmioWrite(0x50, 4, rirbBase); // RIRBLBASE
    mmioWrite(0x54, 4, 0); // RIRBUBASE

    // Set pointers so first command/response lands at entry 0.
    mmioWrite(0x4a, 2, 0x00ff); // CORBRP
    mmioWrite(0x58, 2, 0x00ff); // RIRBWP

    // Enable response interrupts (CIS) + global interrupt enable.
    mmioWrite(0x20, 4, (1 << 31) | (1 << 30)); // INTCTL.GIE | INTCTL.CIE
    mmioWrite(0x5c, 1, 0x03); // RIRBCTL.RUN | RIRBCTL.RINTCTL
    mmioWrite(0x4c, 1, 0x02); // CORBCTL.RUN

    // Submit the 3 commands (CORB[0..2]) then wait for the responses (RIRBWP should advance to 2).
    mmioWrite(0x48, 2, 0x0002); // CORBWP
    let verbsOk = false;
    for (let i = 0; i < 1024; i++) {
      stepFrames(1);
      if ((mmioRead(0x58, 2) & 0xffff) === 0x0002) {
        verbsOk = true;
        break;
      }
    }
    if (!verbsOk) {
      throw new Error("Timed out waiting for HDA CORB/RIRB verb processing.");
    }

    // Program capture stream descriptor 1 (SD#1).
    const sd1Base = 0x80 + 0x20 * 1;
    const streamId = 2;
    const fmtRaw = 0x0010; // 48kHz, 16-bit, mono
    const ctl = (1 << 0) | (1 << 1) | (streamId << 20); // RUN | STRM=2

    mmioWrite(sd1Base + 0x18, 4, bdlBase); // BDPL
    mmioWrite(sd1Base + 0x1c, 4, 0); // BDPU
    mmioWrite(sd1Base + 0x08, 4, pcmBytes); // CBL
    mmioWrite(sd1Base + 0x0c, 2, 0); // LVI
    mmioWrite(sd1Base + 0x12, 2, fmtRaw); // FMT
    mmioWrite(sd1Base + 0x00, 4, ctl); // CTL

    const lpibBefore = mmioRead(sd1Base + 0x04, 4) >>> 0;

    // Advance the device and allow the capture DMA to run.
    stepFrames(1024);

    const lpibAfter = mmioRead(sd1Base + 0x04, 4) >>> 0;

    const pcm = new Uint8Array(pcmBytes);
    pcm.set(guestU8.subarray(pcmBase, pcmBase + pcmBytes));

    // Stop the capture stream so subsequent IO worker ticks (if bus mastering is later enabled)
    // don't continue consuming mic samples after the harness has returned.
    mmioWrite(sd1Base + 0x00, 4, 0);
    // Also stop the CORB/RIRB engines + interrupts so we don't leave the device asserting IRQs
    // or doing extra work after the harness returns.
    mmioWrite(0x4c, 1, 0); // CORBCTL
    mmioWrite(0x5c, 1, 0); // RIRBCTL
    mmioWrite(0x20, 4, 0); // INTCTL
    mmioWrite(0x24, 4, 0xffff_ffff); // INTSTS (RW1C)

    ctx.postMessage(
      {
        type: "hda.micCaptureTest.result",
        requestId,
        ok: true,
        pcm: pcm.buffer,
        lpibBefore,
        lpibAfter,
      } satisfies HdaMicCaptureTestResultMessage,
      [pcm.buffer],
    );
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    ctx.postMessage({ type: "hda.micCaptureTest.result", requestId, ok: false, error: message } satisfies HdaMicCaptureTestResultMessage);
  } finally {
    if (mgr && restorePciCommand !== null && restorePciAddrReg !== null) {
      try {
        mgr.portWrite(0x0cf8, 4, cfgAddrHdaCommand);
        mgr.portWrite(0x0cfc, 2, restorePciCommand & 0xffff);
      } catch {
        // Best-effort; do not mask the harness result.
      } finally {
        try {
          mgr.portWrite(0x0cf8, 4, restorePciAddrReg);
        } catch {
          // ignore
        }
      }
    }
  }
}

function attachAudioRingBuffer(
  ringBuffer: SharedArrayBuffer | null,
  capacityFrames?: number,
  channelCount?: number,
  dstSampleRate?: number,
): void {
  const parsePositiveSafeU32 = (value: unknown): number => {
    if (typeof value !== "number" || !Number.isSafeInteger(value) || value <= 0 || value > 0xffff_ffff) return 0;
    return value >>> 0;
  };

  let cap = parsePositiveSafeU32(capacityFrames);
  let cc = parsePositiveSafeU32(channelCount);
  let sr = parsePositiveSafeU32(dstSampleRate);

  let views: AudioWorkletRingBufferViews | null = null;
  if (ringBuffer !== null) {
    const Sab = globalThis.SharedArrayBuffer;
    if (typeof Sab === "undefined") {
      console.warn("[io.worker] SharedArrayBuffer is unavailable; dropping audio ring attachment.");
      ringBuffer = null;
    } else if (!(ringBuffer instanceof Sab)) {
      console.warn("[io.worker] setAudioRingBuffer expects a SharedArrayBuffer or null; dropping attachment.");
      ringBuffer = null;
    } else if (ringBuffer.byteLength < AUDIO_OUT_HEADER_BYTES) {
      console.warn("[io.worker] audio ring buffer is too small; dropping attachment.");
      ringBuffer = null;
    } else if (cap === 0 || cc === 0 || sr === 0) {
      console.warn("[io.worker] audio ring buffer metadata is invalid; dropping attachment.");
      ringBuffer = null;
    } else {
      try {
        // Validate against the canonical ring buffer layout (also creates convenient views).
        views = wrapAudioOutRingBuffer(ringBuffer, cap, cc);
      } catch (err) {
        console.warn("[io.worker] audio ring buffer wrap failed; dropping attachment:", err);
        ringBuffer = null;
        cap = 0;
        cc = 0;
        sr = 0;
        views = null;
      }
    }
  }

  audioOutRingBuffer = ringBuffer;
  audioOutViews = views;
  audioOutCapacityFrames = ringBuffer ? cap : 0;
  audioOutChannelCount = ringBuffer ? cc : 0;
  audioOutDstSampleRate = ringBuffer ? sr : 0;
  audioOutTelemetryNextMs = 0;

  // If the guest HDA device is active, attach/detach the ring buffer so the WASM-side
  // HDA controller can stream directly into the AudioWorklet output ring. In WASM
  // builds without HDA, fall back to virtio-snd.
  //
  // NOTE: the playback ring is SPSC (single-producer). Ensure only one guest
  // audio device is attached to it at a time.
  const hda = hdaDevice;
  const snd = virtioSndDevice;
  if (hda) {
    try {
      hda.setAudioRingBuffer({
        ringBuffer,
        capacityFrames: audioOutCapacityFrames,
        channelCount: audioOutChannelCount,
        dstSampleRateHz: audioOutDstSampleRate,
      });
    } catch (err) {
      console.warn("[io.worker] HDA setAudioRingBuffer failed:", err);
    }
    // Ensure the capture path stays pinned to the host mic sample rate even if changing the
    // output rate caused the WASM device model to update its capture rate (default tracking).
    if (micSampleRate > 0) {
      try {
        hda.setCaptureSampleRateHz(micSampleRate);
      } catch {
        // ignore
      }
    }
    if (snd) {
      try {
        snd.setAudioRingBuffer({
          ringBuffer: null,
          capacityFrames: audioOutCapacityFrames,
          channelCount: audioOutChannelCount,
          dstSampleRateHz: audioOutDstSampleRate,
        });
      } catch {
        // ignore
      }
    }
  } else if (snd) {
    try {
      snd.setAudioRingBuffer({
        ringBuffer,
        capacityFrames: audioOutCapacityFrames,
        channelCount: audioOutChannelCount,
        dstSampleRateHz: audioOutDstSampleRate,
      });
    } catch (err) {
      console.warn("[io.worker] virtio-snd setAudioRingBuffer failed:", err);
    }
    // Match the capture sample rate to the live host mic graph if one is present.
    if (micSampleRate > 0) {
      try {
        snd.setCaptureSampleRateHz(micSampleRate);
      } catch {
        // ignore
      }
    }
  }
}

function maybePublishAudioOutTelemetry(nowMs: number): void {
  const views = audioOutViews;
  const capacityFrames = audioOutCapacityFrames;
  // The coordinator owns ring-buffer attachment policy so that the AudioWorklet
  // ring remains single-producer/single-consumer (SPSC). When the IO worker is
  // attached to the audio output ring, it is the producer and should publish
  // producer-side telemetry.
  const shouldPublish = !!views && capacityFrames > 0;

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

async function handleVmSnapshotSaveToOpfs(
  path: string,
  cpu: ArrayBuffer,
  mmu: ArrayBuffer,
  coordinatorDevices?: VmSnapshotDeviceBlob[],
): Promise<void> {
  if (snapshotOpInFlight) {
    throw new Error("VM snapshot operation already in progress.");
  }
  snapshotOpInFlight = true;
  try {
    const api = wasmApi;
    if (!api) {
      throw new Error("WASM is not initialized in the IO worker; cannot save VM snapshot.");
    }
    let mergedCoordinatorDevices = coordinatorDevices;
    if (diskClient) {
      const diskState = await diskClient.prepareSnapshot();
      const diskBlob: VmSnapshotDeviceBlob = { kind: IO_WORKER_RUNTIME_DISK_SNAPSHOT_KIND, bytes: diskState.slice().buffer };
      mergedCoordinatorDevices = Array.isArray(coordinatorDevices) ? [...coordinatorDevices, diskBlob] : [diskBlob];
    }

    await saveIoWorkerVmSnapshotToOpfs({
      api,
      path,
      cpu,
      mmu,
      guestBase,
      guestSize,
      vramU8: vramU8 && vramSizeBytes > 0 ? vramU8.subarray(0, vramSizeBytes) : null,
      runtimes: {
        usbXhciControllerBridge: xhciControllerBridge,
        usbUhciRuntime: uhciRuntime,
        usbUhciControllerBridge: uhciControllerBridge,
        usbEhciControllerBridge: ehciControllerBridge,
        i8042: i8042Wasm ?? i8042Ts,
        virtioInputKeyboard,
        virtioInputMouse,
        // Wrap audio devices so snapshot restore semantics (pending bytes + ring reattachment) stay
        // centralized in the IO worker.
        audioHda: {
          save_state: () => snapshotAudioHdaDeviceState()?.bytes,
          load_state: (bytes: Uint8Array) => restoreAudioHdaDeviceState(bytes),
        },
        audioVirtioSnd: {
          save_state: () => snapshotAudioVirtioSndDeviceState()?.bytes,
          load_state: (bytes: Uint8Array) => restoreAudioVirtioSndDeviceState(bytes),
        },
        pciBus: deviceManager?.pciBus ?? null,
        // Wrap net.e1000 so we can preserve IO-worker-side reset hooks (see `E1000PciDevice.onSnapshotRestore()`).
        netE1000: {
          save_state: () => snapshotE1000DeviceState()?.bytes,
          load_state: (bytes: Uint8Array) => restoreE1000DeviceState(bytes),
        },
        netStack: resolveNetStackSnapshotBridge(),
      },
      restoredDevices: snapshotRestoredDeviceBlobs,
      coordinatorDevices: mergedCoordinatorDevices,
    });
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
    const restored = await restoreIoWorkerVmSnapshotFromOpfs({
      api,
      path,
      guestBase,
      guestSize,
      vramU8: vramU8 && vramSizeBytes > 0 ? vramU8.subarray(0, vramSizeBytes) : null,
      runtimes: {
        usbXhciControllerBridge: xhciControllerBridge,
        usbUhciRuntime: uhciRuntime,
        usbUhciControllerBridge: uhciControllerBridge,
        usbEhciControllerBridge: ehciControllerBridge,
        i8042: i8042Wasm ?? i8042Ts,
        virtioInputKeyboard,
        virtioInputMouse,
        audioHda: {
          load_state: (bytes: Uint8Array) => restoreAudioHdaDeviceState(bytes),
        },
        audioVirtioSnd: {
          load_state: (bytes: Uint8Array) => restoreAudioVirtioSndDeviceState(bytes),
        },
        pciBus: deviceManager?.pciBus ?? null,
        netE1000: {
          load_state: (bytes: Uint8Array) => restoreE1000DeviceState(bytes),
        },
        netStack: resolveNetStackSnapshotBridge(),
      },
    });
 
    // WebUSB host actions are backed by JS Promises and cannot be resumed after restoring a VM
    // snapshot. Cancel any in-flight worker-side awaits so the guest can re-emit actions instead of
    // deadlocking on completions that will never arrive.
    if (restored.restoredDevices.some((d) => d.kind === VM_SNAPSHOT_DEVICE_USB_KIND) && usbPassthroughRuntime) {
      try {
        usbPassthroughRuntime.stop();
        usbPassthroughRuntime.start();
      } catch (err) {
        console.warn("[io.worker] Failed to reset WebUSB passthrough runtime after snapshot restore", err);
      }
    }

    if (findRuntimeDiskWorkerSnapshotDeviceBlob(restored.restoredDevices)) {
      if (!diskClient) diskClient = new RuntimeDiskClient();
      const restoredDisk = await restoreRuntimeDiskWorkerSnapshotFromDeviceBlobs({ devices: restored.restoredDevices, diskClient });
      if (restoredDisk) {
        activeDisk = restoredDisk.activeDisk;
        cdDisk = restoredDisk.cdDisk;
      }
    }

    snapshotRestoredDeviceBlobs = restored.restoredDevices;
    return { cpu: restored.cpu, mmu: restored.mmu, devices: restored.devices };
  } finally {
    snapshotOpInFlight = false;
  }
}

async function initWorker(init: WorkerInitMessage): Promise<void> {
  const hostOnly = machineHostOnlyMode;
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

    const hostOnly = machineHostOnlyMode;
    if (hostOnly) {
      perf.spanBegin("worker:init");
      try {
        role = init.role ?? "io";
        const control = init.controlSab!;
        ioIpcSab = init.ioIpcSab!;

        // Host-only mode must not touch guest RAM. Avoid creating any typed array views into
        // `guestMemory.buffer` (even read-only ones); only map the small control/status region.
        status = new Int32Array(control, STATUS_OFFSET_BYTES, STATUS_INTS);
        guestU8 = new Uint8Array(0);
        guestLayout = null;
        guestBase = 0;
        guestSize = 0;
        sharedFramebuffer = null;

        const regions = ringRegionsForWorker(role);
        commandRing = new RingBuffer(control, regions.command.byteOffset);
        eventRing = new RingBuffer(control, regions.event.byteOffset);

        pushEvent({ kind: "log", level: "info", message: "worker ready (machine runtime host-only)" });

        setReadyFlag(status, role, true);
        ctx.postMessage({ type: MessageType.READY, role } satisfies ProtocolMessage);
        if (perf.traceEnabled) perf.instant("boot:worker:ready", "p", { role, mode: "machine" });
      } finally {
        perf.spanEnd("worker:init");
      }

      return;
    }

    let wasmInitFatal = false;
    if (!hostOnly) {
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
        // Probe within the runtime-reserved region (immediately below guest RAM) so the check is
        // side-effect-free from the guest's perspective.
        //
        // The CPU worker runs a similar probe using a distinct word so the two workers can
        // initialize concurrently without racing.
        assertWasmMemoryWiring({ api, memory: init.guestMemory, context: "io.worker" });
        wasmApi = api;
        pendingWasmInit = { api, variant };
        maybeSendWasmReady();
        usbHid = new api.UsbHidBridge();
        maybeInitUhciDevice();
        maybeInitXhciDevice();
        if (currentConfig?.vmRuntime !== "machine") {
          maybeInitVirtioNetDevice();
          if (!virtioNetDevice) maybeInitE1000Device();
        }
        maybeInitVirtioInput();
        maybeInitHdaDevice();
        maybeInitVirtioSndDevice();

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
                if (IS_DEV && msg.type === "usb.demoResult") {
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

        if (IS_DEV && api.WebUsbUhciPassthroughHarness && !usbUhciHarnessRuntime) {
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

        if (IS_DEV && api.WebUsbEhciPassthroughHarness && !usbEhciHarnessRuntime) {
          const ctor = api.WebUsbEhciPassthroughHarness;
          try {
            // EHCI is a high-speed controller: disable the UHCI-only CONFIGURATION→OTHER_SPEED_CONFIGURATION
            // translation in the WebUSB backend so the harness sees the device's real current-speed descriptors.
            //
            // The main-thread UsbBroker routes actions based on the MessagePort they arrive on, so create a
            // dedicated port for the EHCI harness with translation disabled.
            let port: MessagePort | DedicatedWorkerGlobalScope = ctx;
            if (typeof MessageChannel !== "undefined") {
              try {
                const channel = new MessageChannel();
                // Ask the main-thread UsbBroker (listening on `ctx`) to attach the other end with EHCI options.
                ctx.postMessage(
                  {
                    type: "usb.broker.attachPort",
                    port: channel.port2,
                    attachRings: false,
                    backendOptions: { translateOtherSpeedConfigurationDescriptor: false },
                  },
                  [channel.port2],
                );
                port = channel.port1;
                try {
                  // Node/Vitest may keep MessagePorts alive; unref so unit tests don't hang.
                  (channel.port1 as unknown as { unref?: () => void }).unref?.();
                  (channel.port2 as unknown as { unref?: () => void }).unref?.();
                } catch {
                  // ignore
                }
              } catch {
                // Fall back to the default worker channel when MessageChannel is unavailable.
              }
            }

            usbEhciHarnessRuntime = new WebUsbEhciHarnessRuntime({
              createHarness: () => new ctor(),
              port,
              initiallyBlocked: true,
              // Ring handles received on the main worker channel are not compatible with the dedicated harness port.
              initialRingAttach: port === ctx ? (usbRingAttach ?? undefined) : undefined,
              onUpdate: (snapshot) => {
                ctx.postMessage({ type: "usb.ehciHarness.status", snapshot } satisfies UsbEhciHarnessStatusMessage);
              },
            });
          } catch (err) {
            console.warn("[io.worker] Failed to initialize WebUSB EHCI harness runtime", err);
            usbEhciHarnessRuntime = null;
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
            wasmInitFatal = true;
            return;
          }
          console.error(`[io.worker] wasm:init failed: ${message}`);
          pushEvent({ kind: "log", level: "error", message: `wasm:init failed: ${message}` });
        }
      });
      if (wasmInitFatal) return;
    } else {
      // In `vmRuntime=machine` host-only mode this worker should not touch guest RAM or
      // instantiate any guest-visible device models. Skip WASM initialization entirely.
      maybeAnnounceMachineHostOnlyMode();
    }

    perf.spanBegin("worker:init");
    try {
      role = init.role ?? "io";
      const segments = {
        control: init.controlSab!,
        guestMemory: init.guestMemory!,
        vram: init.vram,
        scanoutState: init.scanoutState,
        scanoutStateOffsetBytes: init.scanoutStateOffsetBytes ?? 0,
        cursorState: init.cursorState,
        cursorStateOffsetBytes: init.cursorStateOffsetBytes ?? 0,
        ioIpc: init.ioIpcSab!,
        sharedFramebuffer: init.sharedFramebuffer!,
        sharedFramebufferOffsetBytes: init.sharedFramebufferOffsetBytes ?? 0,
      };
      ioIpcSab = segments.ioIpc;
      const views = createSharedMemoryViews(segments);
      status = views.status;
      guestU8 = views.guestU8;
      vramU8 = views.vramSizeBytes > 0 ? views.vramU8 : null;
      vramBasePaddr = (init.vramBasePaddr ?? VRAM_BASE_PADDR) >>> 0;
      // Guard against mismatched metadata in manually-constructed init messages; always clamp to
      // the actual SharedArrayBuffer size.
      const reportedVramSizeBytes = (init.vramSizeBytes ?? views.vramSizeBytes) >>> 0;
      vramSizeBytes = vramU8 ? Math.min(reportedVramSizeBytes, vramU8.byteLength) >>> 0 : 0;
      guestLayout = views.guestLayout;
      guestBase = views.guestLayout.guest_base >>> 0;
      guestSize = views.guestLayout.guest_size >>> 0;
      sharedFramebuffer = { sab: segments.sharedFramebuffer, offsetBytes: segments.sharedFramebufferOffsetBytes ?? 0 };

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

      if (!hostOnly) {
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
          if (IS_DEV && (flags & IRQ_REFCOUNT_SATURATED) && irqWarnedSaturated[idx] === 0) {
            irqWarnedSaturated[idx] = 1;
            console.warn(`[io.worker] IRQ${idx} refcount saturated at 0xffff (raiseIrq without matching lowerIrq?)`);
          }
        },
        lowerIrq: (irq) => {
          const idx = irq & 0xff;
          const flags = applyIrqRefCountChange(irqRefCounts, idx, false);
          if (flags & IRQ_REFCOUNT_DEASSERT) enqueueIoEvent(encodeEvent({ kind: "irqLower", irq: idx }));
          if (IS_DEV && (flags & IRQ_REFCOUNT_UNDERFLOW) && irqWarnedUnderflow[idx] === 0) {
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

      const hasVram = !!vramU8 && vramSizeBytes > 0;
      const mgr = new DeviceManager(irqSink);
      deviceManager = mgr;

      if (hasVram) {
        const base = BigInt(vramBasePaddr >>> 0);
        const sizeBytes = BigInt(vramSizeBytes >>> 0);
        const vram = vramU8!.subarray(0, vramSizeBytes);
        try {
          // Reserve a dedicated VRAM aperture at the front of the PCI MMIO BAR window so future PCI
          // BAR allocations cannot overlap guest-visible VRAM.
          //
          // If `vramBasePaddr` is overridden above the PCI MMIO base, reserve the entire
          // `[PCI_MMIO_BASE, vramBasePaddr + vramSize)` span so BAR allocation still skips the full
          // mapped VRAM aperture.
          const mmioBase = BigInt(PCI_MMIO_BASE >>> 0);
          const reserveBytes = base >= mmioBase ? base + sizeBytes - mmioBase : sizeBytes;
          mgr.pciBus.reserveMmio(reserveBytes);
        } catch (err) {
          console.warn("[io.worker] Failed to reserve VRAM aperture in PCI MMIO window", err);
        }
        try {
          mgr.registerMmio(base, sizeBytes, new MmioRamHandler(vram));
        } catch (err) {
          console.warn("[io.worker] Failed to map VRAM aperture into MMIO bus", err);
        }
      }

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
      // Minimal AeroGPU PCI device model used to forward the guest-programmed hardware cursor
      // registers (`AEROGPU_MMIO_REG_CURSOR_*`) into the runtime cursor overlay channel.
      //
      // This is intentionally inert until the guest (or a harness) touches the cursor regs so
      // synthetic cursor demos can continue to drive the overlay without being overridden.
      try {
        if (!guestLayout) throw new Error("guestLayout is not initialized");
        const cursorStateWords = views.cursorStateI32;
        const hasVramForCursor = !!vramU8 && vramSizeBytes > 0;
        const vramOpts = hasVramForCursor ? { vramU8: vramU8!, vramBasePaddr, vramSizeBytes } : {};
        aerogpuDevice = new AeroGpuPciDevice(
          cursorStateWords
            ? { guestU8, guestLayout, cursorStateWords, ...vramOpts }
            : {
                guestU8,
                guestLayout,
                ...vramOpts,
                sink: {
                  setImage: (width, height, rgba8) => {
                    ctx.postMessage(
                      { kind: "cursor.set_image", width, height, rgba8 } satisfies CursorSetImageMessage,
                      [rgba8],
                    );
                  },
                  setState: (enabled, x, y, hotX, hotY) => {
                    ctx.postMessage({ kind: "cursor.set_state", enabled, x, y, hotX, hotY } satisfies CursorSetStateMessage);
                  },
                },
              },
        );
        mgr.registerPciDevice(aerogpuDevice);
        mgr.addTickable(aerogpuDevice);
      } catch (err) {
        aerogpuDevice = null;
        console.warn("[io.worker] Failed to initialize AeroGPU cursor forwarding device", err);
      }
      maybeInitUhciDevice();
      maybeInitEhciDevice();
      maybeInitXhciDevice();
      if (currentConfig?.vmRuntime !== "machine") {
        maybeInitVirtioNetDevice();
        if (!virtioNetDevice) maybeInitE1000Device();
      }
      maybeInitVirtioInput();
      maybeInitHdaDevice();
      maybeInitVirtioSndDevice();

      const uart = new Uart16550(UART_COM1, serialSink);
      mgr.registerPortIo(uart.basePort, uart.basePort + 7, uart);

      // If WASM has already finished initializing, install the WebHID passthrough bridge now that
      // we have a device manager (UHCI needs IRQ wiring + PCI registration).
      maybeInitWasmHidGuestBridge();
      } else {
        // Host-only stub: do not create any guest-visible device models.
        deviceManager = null;
      }

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
      //
      // Exception: in `vmRuntime=machine` mode the CPU worker owns disk attachment (and opens
      // the OPFS `FileSystemSyncAccessHandle`). Sync access handles are exclusive, so the IO
      // worker must not open the disk and must not block READY on `setBootDisks`.
      if (!machineHostOnlyMode) {
        await bootDisksInitPromise;
        if (pendingBootDisks) {
          await applyBootDisks(pendingBootDisks);
        }
      } else {
        // Unblock any legacy code paths that still await `bootDisksInitPromise`.
        if (bootDisksInitResolve) {
          bootDisksInitResolve();
          bootDisksInitResolve = null;
        }
        pendingBootDisks = null;
        maybeAnnounceMachineHostOnlyMode();
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

  if (!hostOnly) {
    startIoIpcServer();
  }
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
      | Partial<HdaMicCaptureTestMessage>
      | Partial<HdaCodecDebugStateRequestMessage>
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

      // Machine runtime: the canonical `api.Machine` owns guest device models + guest RAM.
      // Treat the IO worker as a host-only stub and respond to snapshot coordination messages
      // without touching guest/device state.
      if (machineHostOnlyMode) {
        switch (snapshotMsg.kind) {
          case "vm.snapshot.pause": {
            snapshotPaused = true;
            ctx.postMessage({ kind: "vm.snapshot.paused", requestId, ok: true } satisfies VmSnapshotPausedMessage);
            return;
          }
          case "vm.snapshot.resume": {
            snapshotPaused = false;
            ctx.postMessage({ kind: "vm.snapshot.resumed", requestId, ok: true } satisfies VmSnapshotResumedMessage);
            return;
          }
          case "vm.snapshot.saveToOpfs": {
            ctx.postMessage({
              kind: "vm.snapshot.saved",
              requestId,
              ok: false,
              error: serializeVmSnapshotError(new Error("IO worker snapshots are unsupported in machine runtime.")),
            } satisfies VmSnapshotSavedMessage);
            return;
          }
          case "vm.snapshot.restoreFromOpfs": {
            ctx.postMessage({
              kind: "vm.snapshot.restored",
              requestId,
              ok: false,
              error: serializeVmSnapshotError(new Error("IO worker snapshots are unsupported in machine runtime.")),
            } satisfies VmSnapshotRestoredMessage);
            return;
          }
          default:
            return;
        }
      }

      switch (snapshotMsg.kind) {
        case "vm.snapshot.pause": {
          void pauseIoWorkerSnapshotAndDrainDiskIo({
            setSnapshotPaused: (paused) => {
              snapshotPaused = paused;
            },
            setUsbProxyCompletionRingDispatchPaused,
            getDiskIoChain: () => diskIoChain,
            onPaused: () => {
              ctx.postMessage({ kind: "vm.snapshot.paused", requestId, ok: true } satisfies VmSnapshotPausedMessage);
            },
          });
          return;
          }
          case "vm.snapshot.resume": {
            if (machineHostOnlyMode) {
              snapshotPaused = false;
              ctx.postMessage({ kind: "vm.snapshot.resumed", requestId, ok: true } satisfies VmSnapshotResumedMessage);
              return;
            }
            snapshotPaused = false;
            // The host-side microphone producer can continue writing into the mic ring buffer while
            // the VM is snapshot-paused (the IO worker stops consuming). Re-attach the ring on
            // resume so the WASM consumer discards any buffered/stale samples and capture starts
          // from the most recent audio.
          if (micRingBuffer) {
            try {
              attachMicRingBuffer(micRingBuffer, micSampleRate);
            } catch (err) {
              console.warn("[io.worker] Failed to refresh microphone ring buffer after snapshot resume", err);
            }
          }
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
              const devicesRaw = (snapshotMsg as Partial<{ devices: unknown }>).devices;
              const devices: VmSnapshotDeviceBlob[] | undefined = Array.isArray(devicesRaw)
                ? (devicesRaw as VmSnapshotDeviceBlob[])
                : undefined;
              await handleVmSnapshotSaveToOpfs(snapshotMsg.path, snapshotMsg.cpu, snapshotMsg.mmu, devices);
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
      const prevVmRuntime = (currentConfig?.vmRuntime ?? "legacy") === "machine" ? "machine" : "legacy";
      currentConfig = update.config;
      const nextVmRuntime = (currentConfig?.vmRuntime ?? "legacy") === "machine" ? "machine" : "legacy";
      currentConfigVersion = update.version;
      setVmRuntimeFromConfigUpdate(update);
      maybeAnnounceMachineHostOnlyMode();
      ctx.postMessage({ kind: "config.ack", version: currentConfigVersion } satisfies ConfigAckMessage);
      if (prevVmRuntime !== nextVmRuntime) {
        if (nextVmRuntime === "machine") {
          teardownGuestNicDevices();
        } else if (started) {
          // If the VM runtime switches back to the legacy runtime, best-effort re-create the
          // guest NIC models (only supported when the PCI topology has not already been populated
          // with a conflicting NIC).
          maybeInitVirtioNetDevice();
          if (!virtioNetDevice) maybeInitE1000Device();
        }
      }
      return;
    }

    // Shared-memory init handshake.
    if ((data as Partial<WorkerInitMessage>).kind === "init") {
      void initWorker(data as WorkerInitMessage);
      return;
    }

    const bootDisks = normalizeSetBootDisksMessage(data);
    if (bootDisks) {
      if (machineHostOnlyMode) {
        // In machine runtime, disk attachment is owned by the CPU worker. Ignore boot disk
        // open requests here so we don't steal the exclusive OPFS sync access handle.
        if (bootDisksInitResolve) {
          bootDisksInitResolve();
          bootDisksInitResolve = null;
        }
        pendingBootDisks = null;
        maybeAnnounceMachineHostOnlyMode();
        return;
      }
      pendingBootDisks = bootDisks;
      if (bootDisksInitResolve) {
        bootDisksInitResolve();
        bootDisksInitResolve = null;
      }
      if (started && pendingBootDisks) {
        queueDiskIo(() => applyBootDisks(pendingBootDisks!));
      }
      return;
    }

    if (machineHostOnlyMode) {
      // Host-only stub mode: the IO worker should not own guest-visible devices (USB/HID/audio/etc).
      // Ignore all feature messages other than the minimal init/config/snapshot protocol.
      if ((data as Partial<InputBatchMessage>).type === "in:input-batch") {
        const msg = data as Partial<InputBatchMessage> & { recycle?: unknown };
        const buffer = msg.buffer;
        if (buffer instanceof ArrayBuffer && msg.recycle === true) {
          postInputBatchRecycle(buffer);
        }
        machineHostOnlyUnavailable(machineHostOnlyMessageLabel(data));
        return;
      }

      machineHostOnlyUnavailable(machineHostOnlyMessageLabel(data));
      return;
    }

    if ((data as Partial<SetMicrophoneRingBufferMessage>).type === "setMicrophoneRingBuffer") {
      const msg = data as Partial<SetMicrophoneRingBufferMessage>;
      attachMicRingBuffer((msg.ringBuffer as SharedArrayBuffer | null) ?? null, msg.sampleRate);
      return;
    }

    if ((data as Partial<SetAudioRingBufferMessage>).type === "setAudioRingBuffer") {
      const msg = data as Partial<SetAudioRingBufferMessage>;
      attachAudioRingBuffer(
        (msg.ringBuffer as SharedArrayBuffer | null) ?? null,
        msg.capacityFrames,
        msg.channelCount,
        msg.dstSampleRate,
      );
      return;
    }

    // Backwards-compatible alias used by older call sites/docs.
    if ((data as { type?: unknown }).type === "setAudioOutputRingBuffer") {
      const legacy = data as Partial<{
        ringBuffer: SharedArrayBuffer | null;
        sampleRate: number;
        channelCount: number;
        capacityFrames: number;
      }>;
      attachAudioRingBuffer(
        (legacy.ringBuffer as SharedArrayBuffer | null) ?? null,
        legacy.capacityFrames,
        legacy.channelCount,
        legacy.sampleRate,
      );
      return;
    }

    if ((data as Partial<HdaMicCaptureTestMessage>).type === "hda.micCaptureTest") {
      const msg = data as Partial<HdaMicCaptureTestMessage>;
      if (typeof msg.requestId !== "number") return;
      runHdaMicCaptureTest(msg.requestId);
      return;
    }

    if ((data as Partial<HdaCodecDebugStateRequestMessage>).type === "hda.codecDebugState") {
      const msg = data as Partial<HdaCodecDebugStateRequestMessage>;
      const requestId = msg.requestId;
      if (typeof requestId !== "number") return;

      const anyGlobal = globalThis as unknown as Record<string, unknown>;
      // Prefer the explicit global hook exposed by the audio integration (see `web/src/workers/io.worker.ts` init
      // path). Fall back to the live HDA controller bridge if present.
      const bridge = (anyGlobal["__aeroAudioHdaBridge"] as unknown) ?? hdaControllerBridge;
      const fn = (bridge as unknown as { codec_debug_state?: unknown } | null)?.codec_debug_state;
      if (typeof fn !== "function") {
        const res: HdaCodecDebugStateResultMessage = {
          type: "hda.codecDebugStateResult",
          requestId,
          ok: false,
          error: "HDA codec debug state is unavailable (no bridge or missing codec_debug_state export).",
        };
        ctx.postMessage(res);
        return;
      }

      try {
        // codec_debug_state returns a structured-cloneable JS object (plain numbers/bools/strings).
        const state = (fn as () => unknown).call(bridge);
        const res: HdaCodecDebugStateResultMessage = { type: "hda.codecDebugStateResult", requestId, ok: true, state };
        ctx.postMessage(res);
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        const res: HdaCodecDebugStateResultMessage = { type: "hda.codecDebugStateResult", requestId, ok: false, error: message };
        ctx.postMessage(res);
      }
      return;
    }

    if ((data as Partial<HdaSnapshotStateRequestMessage>).type === "hda.snapshotState") {
      const msg = data as Partial<HdaSnapshotStateRequestMessage>;
      const requestId = msg.requestId;
      if (typeof requestId !== "number") return;

      const bridge = resolveAudioHdaSnapshotBridge();
      if (!bridge) {
        const res: HdaSnapshotStateResultMessage = {
          type: "hda.snapshotStateResult",
          requestId,
          ok: false,
          error: "HDA snapshot state is unavailable (no bridge).",
        };
        ctx.postMessage(res);
        return;
      }

      const save =
        (bridge as unknown as { save_state?: unknown }).save_state ?? (bridge as unknown as { snapshot_state?: unknown }).snapshot_state;
      if (typeof save !== "function") {
        const res: HdaSnapshotStateResultMessage = {
          type: "hda.snapshotStateResult",
          requestId,
          ok: false,
          error: "HDA snapshot state export unavailable (missing save_state/snapshot_state).",
        };
        ctx.postMessage(res);
        return;
      }

      try {
        const bytes = save.call(bridge) as unknown;
        if (!(bytes instanceof Uint8Array)) {
          const res: HdaSnapshotStateResultMessage = {
            type: "hda.snapshotStateResult",
            requestId,
            ok: false,
            error: "HDA snapshot export returned unexpected type.",
          };
          ctx.postMessage(res);
          return;
        }
        // Copy so callers always receive a standalone ArrayBuffer-backed view (not a WASM memory view).
        const copy = new Uint8Array(bytes.byteLength);
        copy.set(bytes);
        const res: HdaSnapshotStateResultMessage = { type: "hda.snapshotStateResult", requestId, ok: true, bytes: copy };
        ctx.postMessage(res);
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        const res: HdaSnapshotStateResultMessage = { type: "hda.snapshotStateResult", requestId, ok: false, error: message };
        ctx.postMessage(res);
      }
      return;
    }

    if ((data as Partial<HdaTickStatsRequestMessage>).type === "hda.tickStats") {
      const msg = data as Partial<HdaTickStatsRequestMessage>;
      const requestId = msg.requestId;
      if (typeof requestId !== "number") return;

      const hda = hdaDevice;
      if (!hda) {
        const res: HdaTickStatsResultMessage = {
          type: "hda.tickStatsResult",
          requestId,
          ok: false,
          error: "HDA device is not initialized.",
        };
        ctx.postMessage(res);
        return;
      }

      try {
        const stats = hda.getTickStats();
        const res: HdaTickStatsResultMessage = { type: "hda.tickStatsResult", requestId, ok: true, stats };
        ctx.postMessage(res);
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        const res: HdaTickStatsResultMessage = { type: "hda.tickStatsResult", requestId, ok: false, error: message };
        ctx.postMessage(res);
      }
      return;
    }

    if ((data as Partial<VirtioSndSnapshotStateRequestMessage>).type === "virtioSnd.snapshotState") {
      const msg = data as Partial<VirtioSndSnapshotStateRequestMessage>;
      const requestId = msg.requestId;
      if (typeof requestId !== "number") return;

      const dev = virtioSndDevice;
      if (!dev) {
        const res: VirtioSndSnapshotStateResultMessage = {
          type: "virtioSnd.snapshotStateResult",
          requestId,
          ok: false,
          error: "virtio-snd device is not initialized.",
        };
        ctx.postMessage(res);
        return;
      }

      try {
        const bytes = dev.saveState();
        if (!(bytes instanceof Uint8Array)) {
          const res: VirtioSndSnapshotStateResultMessage = {
            type: "virtioSnd.snapshotStateResult",
            requestId,
            ok: false,
            error: "virtio-snd snapshot export unavailable.",
          };
          ctx.postMessage(res);
          return;
        }
        // Copy so callers always receive a standalone ArrayBuffer-backed view (not a WASM memory view).
        const copy = new Uint8Array(bytes.byteLength);
        copy.set(bytes);
        const res: VirtioSndSnapshotStateResultMessage = { type: "virtioSnd.snapshotStateResult", requestId, ok: true, bytes: copy };
        ctx.postMessage(res);
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        const res: VirtioSndSnapshotStateResultMessage = {
          type: "virtioSnd.snapshotStateResult",
          requestId,
          ok: false,
          error: message,
        };
        ctx.postMessage(res);
      }
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

    if ((data as Partial<UsbEhciHarnessAttachControllerMessage>).type === "usb.ehciHarness.attachController") {
      if (usbEhciHarnessRuntime) {
        usbEhciHarnessRuntime.attachController();
      } else {
        const snapshot: WebUsbEhciHarnessRuntimeSnapshot = {
          available: false,
          blocked: true,
          controllerAttached: false,
          deviceAttached: false,
          tickCount: 0,
          actionsForwarded: 0,
          completionsApplied: 0,
          pendingCompletions: 0,
          irqLevel: false,
          usbSts: 0,
          usbStsUsbInt: false,
          usbStsUsbErrInt: false,
          usbStsPcd: false,
          lastAction: null,
          lastCompletion: null,
          deviceDescriptor: null,
          configDescriptor: null,
          lastError: "WebUsbEhciPassthroughHarness export unavailable (or dev-only harness disabled).",
        };
        ctx.postMessage({ type: "usb.ehciHarness.status", snapshot } satisfies UsbEhciHarnessStatusMessage);
      }
      return;
    }

    if ((data as Partial<UsbEhciHarnessDetachControllerMessage>).type === "usb.ehciHarness.detachController") {
      if (usbEhciHarnessRuntime) {
        usbEhciHarnessRuntime.detachController();
      } else {
        const snapshot: WebUsbEhciHarnessRuntimeSnapshot = {
          available: false,
          blocked: true,
          controllerAttached: false,
          deviceAttached: false,
          tickCount: 0,
          actionsForwarded: 0,
          completionsApplied: 0,
          pendingCompletions: 0,
          irqLevel: false,
          usbSts: 0,
          usbStsUsbInt: false,
          usbStsUsbErrInt: false,
          usbStsPcd: false,
          lastAction: null,
          lastCompletion: null,
          deviceDescriptor: null,
          configDescriptor: null,
          lastError: null,
        };
        ctx.postMessage({ type: "usb.ehciHarness.status", snapshot } satisfies UsbEhciHarnessStatusMessage);
      }
      return;
    }

    if ((data as Partial<UsbEhciHarnessAttachDeviceMessage>).type === "usb.ehciHarness.attachDevice") {
      if (usbEhciHarnessRuntime) {
        usbEhciHarnessRuntime.attachDevice();
      }
      return;
    }

    if ((data as Partial<UsbEhciHarnessDetachDeviceMessage>).type === "usb.ehciHarness.detachDevice") {
      if (usbEhciHarnessRuntime) {
        usbEhciHarnessRuntime.detachDevice();
      }
      return;
    }

    if ((data as Partial<UsbEhciHarnessGetDeviceDescriptorMessage>).type === "usb.ehciHarness.getDeviceDescriptor") {
      if (usbEhciHarnessRuntime) {
        usbEhciHarnessRuntime.runGetDeviceDescriptor();
      }
      return;
    }

    if ((data as Partial<UsbEhciHarnessGetConfigDescriptorMessage>).type === "usb.ehciHarness.getConfigDescriptor") {
      if (usbEhciHarnessRuntime) {
        usbEhciHarnessRuntime.runGetConfigDescriptor();
      }
      return;
    }

    if ((data as Partial<UsbEhciHarnessClearUsbStsMessage>).type === "usb.ehciHarness.clearUsbSts") {
      if (usbEhciHarnessRuntime) {
        const msg = data as Partial<UsbEhciHarnessClearUsbStsMessage>;
        if (typeof msg.bits === "number") {
          usbEhciHarnessRuntime.clearUsbSts(msg.bits);
        }
      }
      return;
    }

    if (isHidRingDetachMessage(data)) {
      const reason = data.reason ?? "HID proxy rings disabled.";
      detachHidRings(reason, { notifyBroker: false });
      return;
    }

    if (isHidRingInitMessage(data)) {
      const msg = data as HidRingInitMessage;
      hidProxyInputRing = new RingBuffer(msg.sab, msg.offsetBytes);
      hidProxyInputRingForwarded = 0;
      hidProxyInputRingInvalid = 0;
      hidRingDetachSent = false;
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
      const msg = data;
      const capture = { deviceId: msg.deviceId, message: null as string | null };
      hidAttachResultCapture = capture;
      try {
        hidGuest.attach(msg);
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        capture.message ??= message;
      } finally {
        hidAttachResultCapture = null;
      }

      const ok = capture.message === null;
      if (!ok) {
        // Best-effort cleanup of any partial state produced by a failed attach (e.g. bridge
        // constructed but topology attach failed after inserting it into the map).
        try {
          hidGuest.detach({ type: "hid.detach", deviceId: msg.deviceId });
        } catch {
          // ignore
        }
      }

      const result: HidAttachResultMessage = {
        type: "hid.attachResult",
        deviceId: msg.deviceId,
        ok,
        ...(capture.message !== null ? { error: capture.message } : {}),
      };
      ctx.postMessage(result);
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

    if (isHidFeatureReportResultMessage(data)) {
      handleHidFeatureReportResult(data);
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

    if (isHidPassthroughFeatureReportResultMessage(data)) {
      const translated = legacyHidAdapter.featureReportResult(data);
      if (translated) {
        handleHidFeatureReportResult(translated);
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

    if (isUsbGuestControllerModeMessage(data)) {
      applyWebUsbGuestControllerMode(data.mode);
      return;
    }

    if (isUsbSelectedMessage(data)) {
      const msg = data;
      usbAvailable = msg.ok;
      lastUsbSelected = msg;
      if (webUsbGuestBridge) {
        try {
          const kind: WebUsbGuestControllerKind = webUsbGuestControllerKind ?? "uhci";
          applyUsbSelectedToWebUsbGuestBridge(kind, webUsbGuestBridge, msg);
          if (kind === "uhci" && uhciRuntimeWebUsbBridge && webUsbGuestBridge === uhciRuntimeWebUsbBridge) {
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
        } else if (wasmApi && !wasmApi.XhciControllerBridge && !wasmApi.UhciControllerBridge && !wasmApi.UhciRuntime) {
          const hasEhci = typeof (wasmApi as unknown as { EhciControllerBridge?: unknown }).EhciControllerBridge === "function";
          if (hasEhci) {
            webUsbGuestLastError = null;
          } else {
            webUsbGuestLastError =
              "USB controller exports unavailable (guest-visible WebUSB passthrough unsupported in this WASM build).";
          }
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
      if (msg.ok && IS_DEV && !usbDemo && !wasmApi?.UhciRuntime) {
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
      if (IS_DEV) {
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

    // Input is delivered via structured `postMessage` to avoid SharedArrayBuffer contention on the
    // main thread and to keep the hot path in JS simple.
    if ((data as Partial<InputBatchMessage>).type === "in:input-batch") {
      const msg = data as Partial<InputBatchMessage>;
      if (!(msg.buffer instanceof ArrayBuffer)) return;
      const buffer = msg.buffer;
      const recycle = (msg as { recycle?: unknown }).recycle === true;

      // Low-overhead input pipeline telemetry (u32 wrap semantics).
      if (status) Atomics.add(status, StatusIndex.IoInputBatchReceivedCounter, 1);

      // Snapshot pause must freeze device-side state so the snapshot contents are deterministic.
      // Queue input while paused and replay after `vm.snapshot.resume`.
      if (snapshotPaused) {
        if (queuedInputBatchBytes + buffer.byteLength <= MAX_QUEUED_INPUT_BATCH_BYTES) {
          queuedInputBatches.push({ buffer, recycle });
          queuedInputBatchBytes += buffer.byteLength;
        } else {
          // Drop excess input to keep memory bounded; best-effort recycle the transferred buffer.
          if (status) Atomics.add(status, StatusIndex.IoInputBatchDropCounter, 1);
          if (recycle) {
            postInputBatchRecycle(buffer);
          }
        }
        return;
      }
      if (started) {
        handleInputBatch(buffer);
      }
      if (recycle) {
        postInputBatchRecycle(buffer);
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
  // In the canonical `api.Machine` runtime, the CPU worker owns all guest device models (PCI, disk,
  // NIC, USB controllers, etc). The IO worker should behave as a host-only stub, so do not start
  // the guest I/O RPC loop.
  if (machineHostOnlyMode) return;
  const cmdRing = ioCmdRing;
  const evtRing = ioEvtRing;
  const mgr = deviceManager;
  if (!cmdRing || !evtRing || !mgr) {
    throw new Error("I/O IPC rings are unavailable; worker was not initialized correctly.");
  }

  started = true;
  ioServerAbort = new AbortController();
  ioServerExitMode = null;
  startAudioOutTelemetryTimer();

  // Publish initial input backend state for debug HUDs/tests (best-effort; the periodic tick will refresh).
  const virtioKeyboardOk = virtioInputKeyboard?.driverOk() ?? false;
  const virtioMouseOk = virtioInputMouse?.driverOk() ?? false;
  maybeInitSyntheticUsbHidDevices();
  maybeUpdateKeyboardInputBackend({ virtioKeyboardOk });
  maybeUpdateMouseInputBackend({ virtioMouseOk });
  publishInputBackendStatus({ virtioKeyboardOk, virtioMouseOk });

  const dispatchTarget: AeroIpcIoDispatchTarget = {
    portRead: (port, size) => {
      if (machineHostOnlyMode) return defaultReadValue(size);
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
      if (machineHostOnlyMode) return;
      try {
        mgr.portWrite(port, size, value);
      } catch {
        // Ignore device errors; still reply so the CPU side doesn't deadlock.
      }
      portWriteCount++;
      if ((portWriteCount & 0xff) === 0) perf.counter("io:portWrites", portWriteCount);
    },
    mmioRead: (addr, size) => {
      if (machineHostOnlyMode) return defaultReadValue(size);
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
      if (machineHostOnlyMode) return;
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
      // Machine runtime host-only mode: do not tick guest devices or interact with shared rings.
      // Keep draining the runtime control ring so shutdown requests are still observed.
      if (machineHostOnlyMode) {
        drainRuntimeCommands();
        if (Atomics.load(status, StatusIndex.StopRequested) === 1) {
          ioServerExitMode = "shutdown";
          ioServerAbort?.abort();
        }
        return;
      }
      const vmNowMs = ioTickTimebase.tick(nowMs, snapshotPaused);

      const perfActive = isPerfActive();
      const t0 = perfActive ? performance.now() : 0;

      // Snapshot pause: freeze device-side state so the coordinator can take a
      // consistent CPU + RAM + device snapshot. Keep draining the runtime control
      // ring so shutdown requests are still observed, but avoid ticking devices.
      if (snapshotPaused) {
        drainRuntimeCommands();
        if (Atomics.load(status, StatusIndex.StopRequested) === 1) {
          ioServerExitMode = "shutdown";
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
        try {
          const res = drainIoHidInputRing(proxyRing, (msg) => hidGuest.inputReport(msg), { throwOnCorrupt: true });
          if (res.forwarded > 0) {
            Atomics.add(status, StatusIndex.IoHidInputReportCounter, res.forwarded);
          }
          if (res.invalid > 0) {
            Atomics.add(status, StatusIndex.IoHidInputReportDropCounter, res.invalid);
          }
          hidProxyInputRingForwarded += res.forwarded;
          hidProxyInputRingInvalid += res.invalid;
          if (IS_DEV && (res.forwarded > 0 || res.invalid > 0) && (hidProxyInputRingForwarded & 0xff) === 0) {
            console.debug(
              `[io.worker] hid.ring.init drained forwarded=${hidProxyInputRingForwarded} invalid=${hidProxyInputRingInvalid}`,
            );
          }
        } catch (err) {
          const message = err instanceof Error ? err.message : String(err);
          detachHidRings(`HID proxy rings disabled: ${message}`);
        }
      }
      mgr.tick(vmNowMs);
      flushSyntheticUsbHidPendingInputReports();
      const virtioKeyboardOk = virtioInputKeyboard?.driverOk() ?? false;
      const virtioMouseOk = virtioInputMouse?.driverOk() ?? false;
      maybeUpdateKeyboardInputBackend({ virtioKeyboardOk });
      maybeUpdateMouseInputBackend({ virtioMouseOk });
      publishInputBackendStatus({ virtioKeyboardOk, virtioMouseOk });
      drainSyntheticUsbHidOutputReports();
      hidGuest.poll?.();
      void usbPassthroughRuntime?.pollOnce();
      usbUhciHarnessRuntime?.pollOnce();
      usbEhciHarnessRuntime?.pollOnce();
      if (usbDemo) {
        try {
          usbDemo.tick();
          usbDemo.pollResults();
        } catch (err) {
          handleUsbDemoFailure("tick", err);
        }
      }

      // Publish HDA tick clamp stats (worker stall observability).
      maybePublishHdaTickTelemetry(nowMs);

      // Publish AudioWorklet-ring producer telemetry when the IO worker is acting
      // as the audio producer (guest HDA device in VM mode).
      maybePublishAudioOutTelemetry(nowMs);

      if (perfActive) perfIoMs += performance.now() - t0;
      maybeEmitPerfSample();

      if (Atomics.load(status, StatusIndex.StopRequested) === 1) {
        ioServerExitMode = "shutdown";
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

    const exitMode = ioServerExitMode;
    ioServerExitMode = null;
    ioServerAbort = null;
    ioServerTask = null;

    if (exitMode === "host-only") {
      // Host-only transition: the worker stays alive, but the guest I/O server stops so it
      // no longer touches shared rings (NET, HID, disk, etc).
      started = false;
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
      ioServerExitMode = "shutdown";
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
  const layout = guestLayout;
  if (!layout) return null;

  const length = len >>> 0;
  if (guestOffset < 0n) return null;
  if (guestOffset > BigInt(Number.MAX_SAFE_INTEGER)) return null;

  // `guestOffset` is a guest physical address. Once PCI/ECAM holes are modeled, guest physical
  // memory is non-contiguous while `guestU8` remains a flat backing store of RAM bytes.
  //
  // Translate guest physical -> backing-store offset (or reject holes/out-of-range).
  const paddr = Number(guestOffset);
  if (!Number.isSafeInteger(paddr) || BigInt(paddr) !== guestOffset) return null;

  try {
    if (!guestRangeInBounds(layout, paddr, length)) return null;
  } catch {
    return null;
  }

  const start = guestPaddrToRamOffset(layout, paddr);
  if (start === null) {
    // `guestRangeInBounds` treats certain zero-length boundary addresses as in-bounds (e.g.
    // `paddr==LOW_RAM_END` or the end of the high-RAM remap window). For disk I/O a zero-length
    // operation is a no-op; return a deterministic empty view.
    if (length !== 0) return null;

    if (layout.guest_size <= LOW_RAM_END) {
      return paddr === layout.guest_size ? guestU8.subarray(layout.guest_size, layout.guest_size) : null;
    }

    const highLen = layout.guest_size - LOW_RAM_END;
    const highEnd = HIGH_RAM_START + highLen;
    if (paddr === highEnd) {
      return guestU8.subarray(layout.guest_size, layout.guest_size);
    }
    if (paddr === LOW_RAM_END) {
      return guestU8.subarray(LOW_RAM_END, LOW_RAM_END);
    }
    return null;
  }

  const end = start + length;
  if (end > guestU8.byteLength) return null;
  return guestU8.subarray(start, end);
}

function diskRead(diskOffset: bigint, len: number, guestOffset: bigint): AeroIpcIoDiskResult | Promise<AeroIpcIoDiskResult> {
  if (machineHostOnlyMode) {
    // Machine runtime owns disk attachment in the CPU worker; the IO worker must not perform any
    // disk DMA into guest RAM (or hold OPFS sync handles).
    return { ok: false, bytes: 0, errorCode: DISK_ERROR_NO_ACTIVE_DISK };
  }
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
        const { readBytes } = await diskReadIntoGuest({
          client,
          handle: disk.handle,
          range,
          sectorSize: disk.sectorSize,
          guestView: view,
        });
        perfIoReadBytes += readBytes;
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
  if (machineHostOnlyMode) {
    return { ok: false, bytes: 0, errorCode: DISK_ERROR_NO_ACTIVE_DISK };
  }
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

  return queueDiskIoResult(async () => {
    const perfActive = isPerfActive();
    const t0 = perfActive ? performance.now() : 0;
    try {
      if (range.byteLength > 0) {
        const { readBytes, writtenBytes } = await diskWriteFromGuest({
          client,
          handle: disk.handle,
          range,
          sectorSize: disk.sectorSize,
          guestView: view,
        });
        perfIoReadBytes += readBytes;
        perfIoWriteBytes += writtenBytes;
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

function updatePressedConsumerUsage(usageId: number, pressed: boolean): void {
  const u = usageId & 0xffff;
  if (u >= pressedConsumerUsages.length) return;
  const prev = pressedConsumerUsages[u] ?? 0;
  if (pressed) {
    if (prev === 0) {
      pressedConsumerUsages[u] = 1;
      pressedConsumerUsageCount += 1;
    }
    return;
  }
  if (prev !== 0) {
    pressedConsumerUsages[u] = 0;
    pressedConsumerUsageCount = Math.max(0, pressedConsumerUsageCount - 1);
  }
}

function maybeUpdateKeyboardInputBackend(opts: { virtioKeyboardOk: boolean }): void {
  keyboardUsbOk = syntheticUsbHidAttached && !!usbHid && safeSyntheticUsbHidConfigured(syntheticUsbKeyboard);

  const force = currentConfig?.forceKeyboardBackend;
  const virtioOk = opts.virtioKeyboardOk && !!virtioInputKeyboard;
  const usbOk = keyboardUsbOk;

  if (force && force !== "auto") {
    const forcedOk = force === "ps2" || (force === "virtio" && virtioOk) || (force === "usb" && usbOk);
    if (!forcedOk && !warnedForcedKeyboardBackendUnavailable.has(force)) {
      warnedForcedKeyboardBackendUnavailable.add(force);
      const reason =
        force === "virtio"
          ? virtioInputKeyboard
            ? "virtio-input keyboard is not ready (DRIVER_OK not set)"
            : "virtio-input keyboard device is unavailable"
          : force === "usb"
            ? !usbHid
              ? "USB HID bridge is unavailable"
              : !syntheticUsbHidAttached
                ? "synthetic USB HID devices are not attached"
                : "synthetic USB keyboard is not configured by the guest yet"
            : "requested backend is unavailable";
      const message = `[io.worker] forceKeyboardBackend=${force} requested, but ${reason}; falling back to auto selection.`;
      console.warn(message);
      pushEvent({ kind: "log", level: "warn", message });
    }
  }

  const prevBackend = keyboardInputBackend;
  const nextBackend = chooseKeyboardInputBackend({
    current: keyboardInputBackend,
    keysHeld: pressedKeyboardHidUsageCount !== 0 || pressedConsumerUsageCount !== 0,
    virtioOk,
    usbOk,
    force,
  });
  if (nextBackend !== prevBackend) {
    // Low-overhead telemetry (u32 wrap semantics).
    if (status) Atomics.add(status, StatusIndex.IoKeyboardBackendSwitchCounter, 1);
  }
  keyboardInputBackend = nextBackend;
}

function maybeUpdateMouseInputBackend(opts: { virtioMouseOk: boolean }): void {
  const ps2Available = !!(i8042Wasm || i8042Ts);
  const syntheticUsbMouseConfigured = syntheticUsbHidAttached && !!usbHid && safeSyntheticUsbHidConfigured(syntheticUsbMouse);
  // Expose "configured" (not merely selected) status for diagnostics/HUDs.
  // When PS/2 is unavailable we may still route input through the USB path
  // before the guest configures the synthetic HID device, but input reports
  // are dropped until configuration completes.
  mouseUsbOk = syntheticUsbMouseConfigured;
  const prevBackend = mouseInputBackend;
  const force = currentConfig?.forceMouseBackend;
  const virtioOk = opts.virtioMouseOk && !!virtioInputMouse;
  const usbOk = !!usbHid && (!ps2Available || syntheticUsbMouseConfigured);
  const nextBackend = chooseMouseInputBackend({
    current: mouseInputBackend,
    buttonsHeld: mouseButtonsMask !== 0,
    virtioOk,
    // Use PS/2 injection until the synthetic USB mouse is configured; once configured, route via
    // the USB HID bridge to avoid duplicate devices in the guest.
    usbOk,
    force,
  });

  if (force && force !== "auto") {
    const forcedOk = force === "ps2" || (force === "virtio" && virtioOk) || (force === "usb" && usbOk);
    if (!forcedOk && !warnedForcedMouseBackendUnavailable.has(force)) {
      warnedForcedMouseBackendUnavailable.add(force);
      const reason =
        force === "virtio"
          ? virtioInputMouse
            ? "virtio-input mouse is not ready (DRIVER_OK not set)"
            : "virtio-input mouse device is unavailable"
          : force === "usb"
            ? !usbHid
              ? "USB HID bridge is unavailable"
              : ps2Available && !syntheticUsbMouseConfigured
                ? "synthetic USB mouse is not configured by the guest yet"
                : "USB mouse backend is unavailable"
            : "requested backend is unavailable";
      const message = `[io.worker] forceMouseBackend=${force} requested, but ${reason}; falling back to auto selection.`;
      console.warn(message);
      pushEvent({ kind: "log", level: "warn", message });
    }
  }

  if (nextBackend !== prevBackend) {
    // Low-overhead telemetry (u32 wrap semantics).
    if (status) Atomics.add(status, StatusIndex.IoMouseBackendSwitchCounter, 1);
  }

  // Optional extra robustness: when we *do* switch backends, send a "buttons=0" update to the
  // previous backend. This should be redundant because backend switching is gated on
  // `mouseButtonsMask===0`, but it provides a safety net in case a prior button-up was dropped.
  if (nextBackend !== prevBackend && mouseButtonsMask === 0) {
    if (prevBackend === "virtio") {
      virtioInputMouse?.injectMouseButtons(0);
    } else if (prevBackend === "usb") {
      usbHid?.mouse_buttons(0);
    } else {
      i8042Wasm?.injectMouseButtons(0);
      i8042Ts?.injectMouseButtons(0);
    }
  }

  mouseInputBackend = nextBackend;
}

function publishInputBackendStatus(opts: { virtioKeyboardOk: boolean; virtioMouseOk: boolean }): void {
  // These values are best-effort debug telemetry only; the emulator must keep running even if
  // Atomics stores fail for any reason (e.g. status not initialized during early boot).
  try {
    Atomics.store(status, StatusIndex.IoInputKeyboardBackend, encodeInputBackendStatus(keyboardInputBackend));
    Atomics.store(status, StatusIndex.IoInputMouseBackend, encodeInputBackendStatus(mouseInputBackend));
    Atomics.store(status, StatusIndex.IoInputVirtioKeyboardDriverOk, opts.virtioKeyboardOk ? 1 : 0);
    Atomics.store(status, StatusIndex.IoInputVirtioMouseDriverOk, opts.virtioMouseOk ? 1 : 0);

    Atomics.store(status, StatusIndex.IoInputUsbKeyboardOk, keyboardUsbOk ? 1 : 0);
    Atomics.store(status, StatusIndex.IoInputUsbMouseOk, mouseUsbOk ? 1 : 0);
    Atomics.store(
      status,
      StatusIndex.IoInputKeyboardHeldCount,
      (pressedKeyboardHidUsageCount + pressedConsumerUsageCount) | 0,
    );
    Atomics.store(status, StatusIndex.IoInputMouseButtonsHeldMask, mouseButtonsMask & 0x1f);
  } catch {
    // ignore (best-effort)
  }
}

function drainSyntheticUsbHidReports(): void {
  const source = usbHid;
  if (!source) return;

  // Lazy-init so older WASM builds (or unit tests) can run without the UHCI/hid-passthrough exports.
  maybeInitSyntheticUsbHidDevices();

  const keyboard = syntheticUsbKeyboard;
  const mouse = syntheticUsbMouse;
  const gamepad = syntheticUsbGamepad;
  const consumer = syntheticUsbConsumerControl;

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

  const consumerConfigured = safeSyntheticUsbHidConfigured(consumer);
  if (consumerConfigured && consumer && syntheticUsbConsumerControlPendingReport) {
    try {
      consumer.push_input_report(0, syntheticUsbConsumerControlPendingReport);
    } catch {
      // ignore
    }
    syntheticUsbConsumerControlPendingReport = null;
  }
  const drainConsumer = source.drain_next_consumer_report;
  if (drainConsumer) {
    for (let i = 0; i < MAX_SYNTHETIC_USB_HID_REPORTS_PER_INPUT_BATCH; i += 1) {
      let report: Uint8Array | null = null;
      try {
        report = drainConsumer.call(source);
      } catch {
        break;
      }
      if (!(report instanceof Uint8Array)) break;
      if (consumerConfigured && consumer) {
        try {
          consumer.push_input_report(0, report);
        } catch {
          // ignore
        }
      } else {
        syntheticUsbConsumerControlPendingReport = report;
      }
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

  const consumer = syntheticUsbConsumerControl;
  if (consumer && syntheticUsbConsumerControlPendingReport && safeSyntheticUsbHidConfigured(consumer)) {
    try {
      consumer.push_input_report(0, syntheticUsbConsumerControlPendingReport);
    } catch {
      // ignore
    }
    syntheticUsbConsumerControlPendingReport = null;
  }
}

function drainSyntheticUsbHidOutputReports(): void {
  // Lazy-init so older WASM builds (or unit tests) can run without the UHCI/hid-passthrough exports.
  maybeInitSyntheticUsbHidDevices();

  const keyboard = syntheticUsbKeyboard;
  const mouse = syntheticUsbMouse;
  const gamepad = syntheticUsbGamepad;
  const consumer = syntheticUsbConsumerControl;

  if (keyboard) drainSyntheticUsbHidOutputReportsForDevice(keyboard);
  if (mouse) drainSyntheticUsbHidOutputReportsForDevice(mouse);
  if (gamepad) drainSyntheticUsbHidOutputReportsForDevice(gamepad);
  if (consumer) drainSyntheticUsbHidOutputReportsForDevice(consumer);
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

let inputBatchDropWarns = 0;
const MAX_INPUT_BATCH_DROP_WARNS = 8;

function noteInputBatchDrop(reason: string, detail: string): void {
  if (!IS_DEV) return;
  if (inputBatchDropWarns >= MAX_INPUT_BATCH_DROP_WARNS) return;
  inputBatchDropWarns += 1;
  console.warn(`[io.worker] Dropping malformed input batch (${reason}): ${detail}`);
  if (inputBatchDropWarns === MAX_INPUT_BATCH_DROP_WARNS) {
    console.warn("[io.worker] Suppressing further malformed input batch warnings.");
  }
}

function handleInputBatch(buffer: ArrayBuffer): void {
  const byteLength = buffer.byteLength >>> 0;
  if (byteLength < INPUT_BATCH_HEADER_BYTES || byteLength % 4 !== 0) {
    invalidInputBatchCount += 1;
    try {
      Atomics.add(status, StatusIndex.IoInputBatchDropCounter, 1);
    } catch {
      // ignore if shared status isn't initialized yet.
    }
    perf.counter("io:inputBatchDrops", invalidInputBatchCount);
    noteInputBatchDrop("invalid-buffer", `byteLength=${byteLength}`);
    return;
  }

  const t0 = performance.now();
  const nowUs = Math.round(t0 * 1000) >>> 0;
  const decoded = validateInputBatchBuffer(buffer);
  if (!decoded.ok) {
    invalidInputBatchCount += 1;
    try {
      Atomics.add(status, StatusIndex.IoInputBatchDropCounter, 1);
    } catch {
      // ignore if shared status isn't initialized yet.
    }
    // `invalidInputBatchCount` is local (non-atomic) but sufficient for perf/debug counters.
    perf.counter("io:inputBatchDrops", invalidInputBatchCount);
    noteInputBatchDrop("invalid-contents", `error=${decoded.error} byteLength=${byteLength}`);
    return;
  }

  // `buffer` is transferred from the main thread, so it is uniquely owned here.
  const { words, count, claimedCount, maxCount } = decoded;
  const batchSendTimestampUs = words[1] >>> 0;
  const batchSendLatencyUs = u32Delta(nowUs, batchSendTimestampUs);

  if (ioInputLatencyMaxWindowStartMs === 0 || t0 - ioInputLatencyMaxWindowStartMs > INPUT_LATENCY_MAX_WINDOW_MS) {
    ioInputLatencyMaxWindowStartMs = t0;
    ioInputBatchSendLatencyMaxUs = 0;
    ioInputEventLatencyMaxUs = 0;
  }
  ioInputBatchSendLatencyEwmaUs =
    ioInputBatchSendLatencyEwmaUs === 0
      ? batchSendLatencyUs
      : Math.round(ioInputBatchSendLatencyEwmaUs + (batchSendLatencyUs - ioInputBatchSendLatencyEwmaUs) * INPUT_LATENCY_EWMA_ALPHA) >>> 0;
  if (batchSendLatencyUs > ioInputBatchSendLatencyMaxUs) {
    ioInputBatchSendLatencyMaxUs = batchSendLatencyUs;
  }

  Atomics.add(status, StatusIndex.IoInputBatchCounter, 1);
  Atomics.add(status, StatusIndex.IoInputEventCounter, count);
  if (count !== claimedCount) {
    invalidInputBatchCount += 1;
    try {
      Atomics.add(status, StatusIndex.IoInputBatchDropCounter, 1);
    } catch {
      // ignore if shared status isn't initialized yet.
    }
    perf.counter("io:inputBatchDrops", invalidInputBatchCount);
    noteInputBatchDrop(
      "clamped-count",
      `claimed=${claimedCount} max=${maxCount} cap=${MAX_INPUT_EVENTS_PER_BATCH} processed=${count} byteLength=${byteLength}`,
    );
  }

  if (count === 0) {
    perfIoReadBytes += byteLength;
    perfIoMs += performance.now() - t0;
    return;
  }

  const virtioKeyboard = virtioInputKeyboard;
  const virtioMouse = virtioInputMouse;
  const virtioKeyboardOk = virtioKeyboard?.driverOk() ?? false;
  const virtioMouseOk = virtioMouse?.driverOk() ?? false;

  // Ensure synthetic USB HID devices exist (when supported) before processing this batch so we
  // can consistently decide whether to use the legacy PS/2 scancode injection path.
  maybeInitSyntheticUsbHidDevices();
  maybeUpdateKeyboardInputBackend({ virtioKeyboardOk });
  maybeUpdateMouseInputBackend({ virtioMouseOk });

  const base = INPUT_BATCH_HEADER_WORDS;
  let eventLatencySumUs = 0;
  let eventLatencyMaxUsBatch = 0;
  for (let i = 0; i < count; i++) {
    const off = base + i * INPUT_BATCH_WORDS_PER_EVENT;
    const type = words[off] >>> 0;
    const eventTimestampUs = words[off + 1] >>> 0;
    const eventLatencyUs = u32Delta(nowUs, eventTimestampUs);
    eventLatencySumUs += eventLatencyUs;
    if (eventLatencyUs > eventLatencyMaxUsBatch) {
      eventLatencyMaxUsBatch = eventLatencyUs;
    }
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
      case InputEventType.HidUsage16: {
        const a = words[off + 2] >>> 0;
        const usagePage = a & 0xffff;
        const pressed = ((a >>> 16) & 1) !== 0;
        const usageId = words[off + 3] & 0xffff;
        // Consumer Control (0x0C) can be delivered either via:
        // - virtio-input keyboard (media keys subset, exposed by the Win7 virtio-input driver as a Consumer Control collection), or
        // - a dedicated synthetic USB HID consumer-control device (supports the full usage ID range).
        if (usagePage === 0x0c) {
          updatePressedConsumerUsage(usageId, pressed);
          // Prefer virtio-input when the virtio keyboard backend is active and the usage is representable as a Linux key code.
          if (keyboardInputBackend === "virtio" && virtioKeyboardOk && virtioKeyboard) {
            const keyCode = hidConsumerUsageToLinuxKeyCode(usageId);
            if (keyCode !== null) {
              try {
                virtioKeyboard.injectKey(keyCode, pressed);
              } catch {
                // ignore
              }
              break;
            }
          }

          // Otherwise fall back to the synthetic USB consumer-control device (when available). This handles browser
          // navigation keys (AC Back/Forward/etc.) which are not currently modeled by the virtio-input keyboard.
          try {
            usbHid?.consumer_event?.(usageId, pressed);
          } catch {
            // ignore
          }
        }
        break;
      }
      case InputEventType.MouseMove: {
        const dx = words[off + 2] | 0;
        const dyPs2 = words[off + 3] | 0;
        // Input batches use PS/2 convention: positive Y is up. virtio-input and HID use +Y down.
        const dyDown = negateI32Saturating(dyPs2);
        if (mouseInputBackend === "virtio") {
          if (virtioMouseOk && virtioMouse) {
            virtioMouse.injectRelMove(dx, dyDown);
          }
        } else if (mouseInputBackend === "ps2") {
          if (i8042Wasm) {
            i8042Wasm.injectMouseMove(dx, dyPs2);
          } else if (i8042Ts) {
            i8042Ts.injectMouseMove(dx, dyPs2);
          }
        } else {
          usbHid?.mouse_move(dx, dyDown);
        }
        break;
      }
      case InputEventType.MouseButtons: {
        const buttons = words[off + 2] & 0xff;
        mouseButtonsMask = buttons;
        if (mouseInputBackend === "virtio") {
          if (virtioMouseOk && virtioMouse) {
            virtioMouse.injectMouseButtons(buttons);
          }
        } else {
          const buttonsClamped = buttons & 0x1f;
          if (mouseInputBackend === "ps2") {
            if (i8042Wasm) {
              i8042Wasm.injectMouseButtons(buttonsClamped);
            } else if (i8042Ts) {
              i8042Ts.injectMouseButtons(buttonsClamped);
            }
          } else {
            usbHid?.mouse_buttons(buttonsClamped);
          }
        }
        break;
      }
      case InputEventType.MouseWheel: {
        const dz = words[off + 2] | 0;
        const dx = words[off + 3] | 0;
        if (mouseInputBackend === "virtio") {
          if (virtioMouseOk && virtioMouse) {
            if (dz !== 0 || dx !== 0) virtioMouse.injectWheel2(dz, dx);
          }
        } else if (mouseInputBackend === "ps2") {
          if (dz !== 0) {
            if (i8042Wasm) {
              i8042Wasm.injectMouseWheel(dz);
            } else if (i8042Ts) {
              i8042Ts.injectMouseWheel(dz);
            }
          }
        } else {
          if (dz !== 0 || dx !== 0) {
            // Prefer a combined wheel2 API when available so scroll events can be represented as a
            // single HID report (matching `InputEventType.MouseWheel`, which carries both axes).
            if (usbHid?.mouse_wheel2) {
              usbHid?.mouse_wheel2?.(dz, dx);
            } else {
              if (dz !== 0) {
                usbHid?.mouse_wheel(dz);
              }
              if (dx !== 0) {
                usbHid?.mouse_hwheel?.(dx);
              }
            }
          }
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
            i8042Ts.injectKeyScancodePacked(packed, len);
          }
        }
        break;
      }
      default:
        // Unknown event type; ignore.
        break;
    }
  }

  const eventLatencyAvgUs = count > 0 ? (Math.round(eventLatencySumUs / count) >>> 0) : 0;
  ioInputEventLatencyEwmaUs =
    ioInputEventLatencyEwmaUs === 0
      ? eventLatencyAvgUs
      : Math.round(ioInputEventLatencyEwmaUs + (eventLatencyAvgUs - ioInputEventLatencyEwmaUs) * INPUT_LATENCY_EWMA_ALPHA) >>> 0;
  if (eventLatencyMaxUsBatch > ioInputEventLatencyMaxUs) {
    ioInputEventLatencyMaxUs = eventLatencyMaxUsBatch;
  }

  Atomics.store(status, StatusIndex.IoInputBatchSendLatencyUs, batchSendLatencyUs | 0);
  Atomics.store(status, StatusIndex.IoInputBatchSendLatencyEwmaUs, ioInputBatchSendLatencyEwmaUs | 0);
  Atomics.store(status, StatusIndex.IoInputBatchSendLatencyMaxUs, ioInputBatchSendLatencyMaxUs | 0);
  Atomics.store(status, StatusIndex.IoInputEventLatencyAvgUs, eventLatencyAvgUs | 0);
  Atomics.store(status, StatusIndex.IoInputEventLatencyEwmaUs, ioInputEventLatencyEwmaUs | 0);
  Atomics.store(status, StatusIndex.IoInputEventLatencyMaxUs, ioInputEventLatencyMaxUs | 0);

  // Re-evaluate backend selection after processing this batch; key-up events can make it safe to
  // transition away from PS/2 scancode injection.
  maybeUpdateKeyboardInputBackend({ virtioKeyboardOk });
  maybeUpdateMouseInputBackend({ virtioMouseOk });
  publishInputBackendStatus({ virtioKeyboardOk, virtioMouseOk });

  // Forward newly queued USB HID reports into the guest-visible UHCI USB HID devices.
  drainSyntheticUsbHidReports();

  perfIoReadBytes += byteLength;
  perfIoMs += performance.now() - t0;
}

function shutdown(): void {
  if (shuttingDown) return;
  shuttingDown = true;
  ioServerAbort?.abort();
  if (audioOutTelemetryTimer !== undefined) {
    clearInterval(audioOutTelemetryTimer);
    audioOutTelemetryTimer = undefined;
  }
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
      syntheticUsbConsumerControl?.free();
      syntheticUsbConsumerControl = null;
      syntheticUsbHidAttached = false;
      syntheticUsbKeyboardPendingReport = null;
      syntheticUsbGamepadPendingReport = null;
      syntheticUsbConsumerControlPendingReport = null;
      keyboardInputBackend = "ps2";
      pressedKeyboardHidUsages.fill(0);
      pressedKeyboardHidUsageCount = 0;
      pressedConsumerUsages.fill(0);
      pressedConsumerUsageCount = 0;
      mouseInputBackend = "ps2";
      mouseButtonsMask = 0;

      webUsbGuestBridge = null;
      webUsbGuestControllerKind = null;

      if (usbPassthroughRuntime) {
        usbPassthroughRuntime.destroy();
        usbPassthroughRuntime = null;
      }

      usbUhciHarnessRuntime?.destroy();
      usbUhciHarnessRuntime = null;
      usbEhciHarnessRuntime?.destroy();
      usbEhciHarnessRuntime = null;
      uhciDevice?.destroy();
      uhciDevice = null;
      ehciDevice?.destroy();
      ehciDevice = null;
      xhciDevice?.destroy();
      xhciDevice = null;
      virtioNetDevice?.destroy();
      virtioNetDevice = null;
      uhciControllerBridge = null;
      ehciControllerBridge = null;
      xhciControllerBridge = null;
      e1000Device?.destroy();
      e1000Device = null;
      e1000Bridge = null;
      virtioInputKeyboard?.destroy();
      virtioInputKeyboard = null;
      virtioInputMouse?.destroy();
      virtioInputMouse = null;
      uhciHidTopology.setUhciBridge(null);
      uhciHidTopologyBridgeSource = null;
      xhciHidTopologyBridge = null;
      xhciHidTopologyBridgeSource = null;
      xhciHidTopology.setXhciBridge(null);
      hdaDevice?.destroy();
      hdaDevice = null;
      hdaControllerBridge = null;
      audioHdaBridge = null;
      pendingAudioHdaSnapshotBytes = null;
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
