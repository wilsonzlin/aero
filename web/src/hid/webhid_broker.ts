import { WebHidPassthroughManager } from "../platform/webhid_passthrough";
import { alignUp, RECORD_ALIGN, ringCtrl } from "../ipc/layout";
import { RingBuffer } from "../ipc/ring_buffer";
import { StatusIndex } from "../runtime/shared_layout";
import { formatOneLineError } from "../text";
import { normalizeCollections, type NormalizedHidCollectionInfo } from "./webhid_normalize";
import {
  computeFeatureReportPayloadByteLengths,
  computeInputReportPayloadByteLengths,
  computeMaxOutputReportBytesOnWire,
  computeOutputReportPayloadByteLengths,
} from "./hid_report_sizes";
import {
  isHidAttachResultMessage,
  isHidErrorMessage,
  isHidGetFeatureReportMessage,
  isHidLogMessage,
  isHidRingDetachMessage,
  isHidSendReportMessage,
  type HidAttachMessage,
  type HidAttachResultMessage,
  type HidDetachMessage,
  type HidFeatureReportResultMessage,
  type HidGetFeatureReportMessage,
  type HidInputReportMessage,
  type HidProxyMessage,
  type HidRingAttachMessage,
  type HidRingDetachMessage,
  type HidRingInitMessage,
  type HidSendReportMessage,
} from "./hid_proxy_protocol";
import type { GuestUsbPath } from "../platform/hid_passthrough_protocol";
import {
  createHidReportRingBuffer,
  HID_REPORT_RECORD_ALIGN,
  HidReportRing,
  HidReportType as HidRingReportType,
} from "../usb/hid_report_ring";
import {
  HID_INPUT_REPORT_RECORD_MAGIC,
  HID_INPUT_REPORT_RECORD_HEADER_BYTES,
  HID_INPUT_REPORT_RECORD_VERSION,
} from "./hid_input_report_ring";

const LEGACY_HID_INPUT_RING_CAPACITY_BYTES = 64 * 1024;
const DEFAULT_HID_INPUT_REPORT_RING_CAPACITY_BYTES = 2 * 1024 * 1024;
/**
 * Hard cap to avoid accidental multi-gigabyte SharedArrayBuffer allocations when
 * a caller passes a bogus value.
 */
const MAX_HID_INPUT_REPORT_RING_CAPACITY_BYTES = 16 * 1024 * 1024;

/**
 * Default capacity for the worker->main thread HID output/feature report ring.
 *
 * The previous 64KiB default was too small to reliably carry feature reports and
 * to buffer bursts when the main thread is momentarily busy (GC, rendering, etc).
 *
 * Keep this bounded and deterministic: this is a fixed-size SharedArrayBuffer.
 */
const DEFAULT_HID_OUTPUT_RING_CAPACITY_BYTES = 1024 * 1024;

/**
 * Hard cap to avoid accidental multi-gigabyte SharedArrayBuffer allocations when
 * a caller passes a bogus value.
 */
const MAX_HID_OUTPUT_RING_CAPACITY_BYTES = 16 * 1024 * 1024;

/**
 * Fail attaches that never produce a worker-side acknowledgement. This guards
 * against version skew (old worker) and worker-side failures that drop the
 * message loop without emitting `hid.attachResult`.
 */
const DEFAULT_HID_ATTACH_RESULT_TIMEOUT_MS = 10_000;

function assertValidHidRingCapacityBytes(name: string, capacityBytes: number): number {
  if (!Number.isSafeInteger(capacityBytes) || capacityBytes <= 0) {
    throw new Error(`invalid ${name}: ${capacityBytes}`);
  }
  if (capacityBytes > 0xffff_ffff) {
    throw new Error(`invalid ${name}: ${capacityBytes}`);
  }
  const cap = capacityBytes >>> 0;
  if (cap > MAX_HID_OUTPUT_RING_CAPACITY_BYTES) {
    throw new Error(`${name} must be <= ${MAX_HID_OUTPUT_RING_CAPACITY_BYTES}`);
  }
  return alignUp(cap, HID_REPORT_RECORD_ALIGN);
}

function assertValidInputReportRingCapacityBytes(name: string, capacityBytes: number): number {
  if (!Number.isSafeInteger(capacityBytes) || capacityBytes <= 0) {
    throw new Error(`invalid ${name}: ${capacityBytes}`);
  }
  if (capacityBytes > 0xffff_ffff) {
    throw new Error(`invalid ${name}: ${capacityBytes}`);
  }
  const cap = capacityBytes >>> 0;
  if (cap > MAX_HID_INPUT_REPORT_RING_CAPACITY_BYTES) {
    throw new Error(`${name} must be <= ${MAX_HID_INPUT_REPORT_RING_CAPACITY_BYTES}`);
  }
  return alignUp(cap, RECORD_ALIGN);
}

export type WebHidBrokerState = {
  workerAttached: boolean;
  attachedDeviceIds: number[];
};

export type WebHidBrokerListener = (state: WebHidBrokerState) => void;

export type WebHidLastInputReportInfo = {
  tsMs: number;
  byteLength: number;
};

export type WebHidInputReportRingStats = Readonly<{
  enabled: boolean;
  pushed: number;
  dropped: number;
  fallback: number;
}>;

export type WebHidOutputSendStats = Readonly<{
  /**
   * Maximum queued (not yet running) sends per device.
   */
  maxPendingDeviceSends: number;
  /**
   * Legacy alias for {@link maxPendingDeviceSends}.
   */
  maxPendingSendsPerDevice: number;
  /**
   * Maximum total bytes of queued (not yet running) report payloads per device.
   *
   * Tasks retain their report payload buffers until they begin executing, so this
   * bounds memory growth when `sendReport`/`sendFeatureReport` is stalled.
   */
  maxPendingSendBytesPerDevice: number;
  /**
   * Optional global cap across all devices. When unset, this is `null`.
   */
  maxPendingSendsTotal: number | null;
  pendingTotal: number;
  pendingBytesTotal: number;
  droppedTotal: number;
  devices: ReadonlyArray<
    Readonly<{
      deviceId: number;
      pending: number;
      pendingBytes: number;
      dropped: number;
    }>
  >;
}>;

type HidDeviceSendTask = () => Promise<void>;
type HidDeviceSendTaskFactory = () => HidDeviceSendTask;
type HidQueuedDeviceSendTask = {
  bytes: number;
  run: HidDeviceSendTask;
};

function computeHasInterruptOut(collections: NormalizedHidCollectionInfo[]): boolean {
  // Full-speed USB interrupt endpoints cannot transfer more than 64 bytes in a single packet.
  // If the device defines any output report larger than that, the guest must omit the interrupt
  // OUT endpoint and fall back to control SET_REPORT(Output) transfers.
  const maxOutputBytes = computeMaxOutputReportBytesOnWire(collections);
  return maxOutputBytes > 0 && maxOutputBytes <= 64;
}

function ensureArrayBufferBacked(bytes: Uint8Array): Uint8Array<ArrayBuffer> {
  // TypeScript's `BufferSource` type excludes `SharedArrayBuffer` in some lib.dom
  // versions, even though Chromium accepts it for WebHID calls. Keep this module
  // strict-friendly by copying when the buffer is shared.
  if (bytes.buffer instanceof ArrayBuffer) return bytes as unknown as Uint8Array<ArrayBuffer>;
  const out = new Uint8Array(bytes.byteLength);
  out.set(bytes);
  return out;
}

const UNKNOWN_INPUT_REPORT_HARD_CAP_BYTES = 64;
const UNKNOWN_FEATURE_REPORT_HARD_CAP_BYTES = 4096;
// WebHID output/feature reports can be transferred over the control endpoint (SET_REPORT / GET_REPORT),
// whose `wLength` field is a 16-bit unsigned integer. When report IDs are in use (reportId != 0),
// the on-wire report also includes a 1-byte reportId prefix, so the payload must be <= 0xfffe.
const MAX_HID_CONTROL_TRANSFER_BYTES = 0xffff;

function maxHidControlPayloadBytes(reportId: number): number {
  return (reportId >>> 0) === 0 ? MAX_HID_CONTROL_TRANSFER_BYTES : MAX_HID_CONTROL_TRANSFER_BYTES - 1;
}

function toU32OrZero(value: number | undefined): number {
  if (typeof value !== "number" || !Number.isFinite(value)) return 0;
  return Math.max(0, Math.floor(value)) >>> 0;
}

function assertPositiveSafeInteger(name: string, value: number): number {
  if (!Number.isSafeInteger(value) || value <= 0) {
    throw new Error(`invalid ${name}: ${value}`);
  }
  return value;
}

// Keep this large enough to absorb bursts, but bounded so a stalled WebHID send
// can't cause unbounded memory growth if the guest spams reports.
const DEFAULT_MAX_PENDING_SENDS_PER_DEVICE = 1024;
const DEFAULT_MAX_PENDING_SEND_BYTES_PER_DEVICE = 4 * 1024 * 1024;
const OUTPUT_SEND_DROP_WARN_INTERVAL_MS = 5000;
// Bound background output ring draining so a busy or malicious guest can't keep
// the main thread spinning in the drain loop (starving UI/rendering). When the
// ring is not being drained to satisfy an ordering barrier (outputRingTail),
// stop after this many records and resume on the next tick.
const MAX_HID_OUTPUT_RING_RECORDS_PER_DRAIN_TICK = 256;
// Approximate byte budget for background drains. The worker can generate output
// reports up to a single control transfer (<= 64KiB payload). When the output
// ring capacity is configured larger than the default, draining too many large
// records in one tick can create large transient allocations on the main thread.
const MAX_HID_OUTPUT_RING_BYTES_PER_DRAIN_TICK = 1024 * 1024;

export class WebHidBroker {
  readonly manager: WebHidPassthroughManager;

  #workerPort: MessagePort | Worker | null = null;
  #workerPortListener: EventListener | null = null;

  #inputReportRing: RingBuffer | null = null;
  #inputReportRingPushed = 0;
  #inputReportRingDropped = 0;
  #inputReportFallback = 0;
  readonly #inputReportRingCapacityBytes: number;
  #status: Int32Array | null = null;

  #nextDeviceId = 1;
  readonly #deviceIdByDevice = new Map<HIDDevice, number>();
  readonly #deviceById = new Map<number, HIDDevice>();

  // The IO worker can request output/feature reports via both `hid.sendReport` structured messages
  // and the SharedArrayBuffer output ring. Chromium's WebHID implementation expects report sends to
  // be serialized per device, so maintain an explicit FIFO per `deviceId` so reports are executed
  // in guest order without stalling other devices.
  readonly #pendingDeviceSends = new Map<number, HidQueuedDeviceSendTask[]>();
  #pendingDeviceSendTotal = 0;
  #pendingDeviceSendBytesTotal = 0;
  readonly #pendingDeviceSendBytesByDevice = new Map<number, number>();
  readonly #maxPendingSendsPerDevice: number;
  readonly #maxPendingSendBytesPerDevice: number;
  readonly #maxPendingSendsTotal: number;
  #outputSendDropped = 0;
  readonly #outputSendDroppedByDevice = new Map<number, number>();
  readonly #outputSendDropWarnedAtByDevice = new Map<number, number>();
  // Track the currently-running queue runner per device using a monotonically increasing token so
  // a runner from a previous device session (e.g. after detach/reattach) cannot clear the running
  // flag for a newer runner.
  readonly #deviceSendTokenById = new Map<number, number>();
  #nextDeviceSendToken = 1;

  readonly #attachedToWorker = new Set<number>();
  readonly #inputReportListeners = new Map<number, (event: HIDInputReportEvent) => void>();
  readonly #lastInputReportInfo = new Map<number, WebHidLastInputReportInfo>();
  readonly #inputReportExpectedPayloadBytes = new Map<number, Map<number, number>>();
  readonly #inputReportSizeWarned = new Set<string>();
  readonly #featureReportExpectedPayloadBytes = new Map<number, Map<number, number>>();
  readonly #featureReportSizeWarned = new Set<string>();
  readonly #outputReportExpectedPayloadBytes = new Map<number, Map<number, number>>();
  readonly #sendReportSizeWarned = new Set<string>();
  #inputReportTruncated = 0;
  #inputReportPadded = 0;
  #inputReportHardCapped = 0;
  #inputReportUnknownSize = 0;

  readonly #pendingAttachResults = new Map<
    number,
    {
      worker: MessagePort | Worker;
      promise: Promise<void>;
      resolve: () => void;
      reject: (err: Error) => void;
      timeout: ReturnType<typeof setTimeout> | null;
    }
  >();

  readonly #listeners = new Set<WebHidBrokerListener>();

  #inputReportEmitTimer: ReturnType<typeof setTimeout> | null = null;

  #inputRing: HidReportRing | null = null;
  #outputRing: HidReportRing | null = null;
  #outputRingDrainTimer: ReturnType<typeof setInterval> | null = null;
  readonly #outputRingCapacityBytes: number;
  readonly #attachResultTimeoutMs: number;
  #ringDetachSent = false;

  #managerUnsubscribe: (() => void) | null = null;
  #prevManagerAttached = new Set<HIDDevice>();

  constructor(
    options: {
      manager?: WebHidPassthroughManager;
      inputReportRingCapacityBytes?: number;
      outputRingCapacityBytes?: number;
      /**
       * Maximum number of queued (not yet running) send/feature tasks per device.
       *
       * WebHID requires per-device serialization, so if an in-flight
       * `sendReport`/`sendFeatureReport`/`receiveFeatureReport` stalls and the
       * guest keeps sending, the queue can otherwise grow without bound.
       *
       * Drop policy when the queue is full: drop newest tasks. This preserves the
       * FIFO ordering for already-queued reports.
       */
      maxPendingDeviceSends?: number;
      /**
       * Maximum total byte size of queued (not yet running) report payloads per
       * device.
       *
       * WebHID output/feature reports can be up to 64KiB each (USB control
       * transfers). If a send stalls and the guest keeps producing reports, we
       * must bound retained memory even if the queue length limit is high.
       */
      maxPendingSendBytesPerDevice?: number;
      /**
       * Legacy name for {@link maxPendingDeviceSends}.
       */
      maxPendingSendsPerDevice?: number;
      maxPendingSendsTotal?: number;
      attachResultTimeoutMs?: number;
    } = {},
  ) {
    this.manager = options.manager ?? new WebHidPassthroughManager();
    this.#inputReportRingCapacityBytes =
      options.inputReportRingCapacityBytes === undefined
        ? DEFAULT_HID_INPUT_REPORT_RING_CAPACITY_BYTES
        : assertValidInputReportRingCapacityBytes("inputReportRingCapacityBytes", options.inputReportRingCapacityBytes);
    this.#outputRingCapacityBytes =
      options.outputRingCapacityBytes === undefined
        ? DEFAULT_HID_OUTPUT_RING_CAPACITY_BYTES
        : assertValidHidRingCapacityBytes("outputRingCapacityBytes", options.outputRingCapacityBytes);
    this.#attachResultTimeoutMs = (() => {
      const requested = options.attachResultTimeoutMs;
      if (requested === undefined) return DEFAULT_HID_ATTACH_RESULT_TIMEOUT_MS;
      if (!Number.isFinite(requested) || requested <= 0) {
        throw new Error(`invalid attachResultTimeoutMs: ${requested}`);
      }
      // Clamp to a sane upper bound; this is a UI-facing await and should never
      // hang indefinitely.
      return Math.min(5 * 60_000, Math.floor(requested)) >>> 0;
    })();
    const maxPendingDeviceSends = options.maxPendingDeviceSends;
    const maxPendingSendsPerDevice = options.maxPendingSendsPerDevice;
    if (
      maxPendingDeviceSends !== undefined &&
      maxPendingSendsPerDevice !== undefined &&
      maxPendingDeviceSends !== maxPendingSendsPerDevice
    ) {
      throw new Error("maxPendingDeviceSends and maxPendingSendsPerDevice must match");
    }
    const maxPending = maxPendingDeviceSends ?? maxPendingSendsPerDevice;
    this.#maxPendingSendsPerDevice =
      maxPending === undefined ? DEFAULT_MAX_PENDING_SENDS_PER_DEVICE : assertPositiveSafeInteger("maxPendingDeviceSends", maxPending);
    this.#maxPendingSendBytesPerDevice = (() => {
      const requested = options.maxPendingSendBytesPerDevice;
      if (requested === undefined) return DEFAULT_MAX_PENDING_SEND_BYTES_PER_DEVICE;
      if (!Number.isSafeInteger(requested) || requested <= 0) {
        throw new Error(`invalid maxPendingSendBytesPerDevice: ${requested}`);
      }
      return requested;
    })();
    this.#maxPendingSendsTotal =
      options.maxPendingSendsTotal === undefined
        ? Number.POSITIVE_INFINITY
        : assertPositiveSafeInteger("maxPendingSendsTotal", options.maxPendingSendsTotal);

    // Ensure we clean up bridged state when the underlying manager closes a device
    // (e.g., after a physical disconnect).
    this.#prevManagerAttached = new Set(this.manager.getState().attachedDevices.map((entry) => entry.device));
    this.#managerUnsubscribe = this.manager.subscribe((state) => {
      const next = new Set(state.attachedDevices.map((entry) => entry.device));
      for (const device of this.#prevManagerAttached) {
        if (!next.has(device)) {
          void this.#handleManagerDeviceDetached(device);
        }
      }
      this.#prevManagerAttached = next;
    });
  }

  destroy(): void {
    this.detachWorkerPort(this.#workerPort ?? undefined);
    this.#managerUnsubscribe?.();
    this.#managerUnsubscribe = null;
    if (this.#inputReportEmitTimer) {
      clearTimeout(this.#inputReportEmitTimer);
      this.#inputReportEmitTimer = null;
    }
    this.#listeners.clear();
  }

  getState(): WebHidBrokerState {
    return {
      workerAttached: !!this.#workerPort,
      attachedDeviceIds: Array.from(this.#attachedToWorker),
    };
  }

  subscribe(listener: WebHidBrokerListener): () => void {
    this.#listeners.add(listener);
    listener(this.getState());
    return () => {
      this.#listeners.delete(listener);
    };
  }

  isWorkerAttached(): boolean {
    return !!this.#workerPort;
  }

  setInputReportRing(ring: RingBuffer | null, status: Int32Array | null = null): void {
    if (ring && ring !== this.#inputReportRing) {
      ring.reset();
    }
    this.#inputReportRing = ring;
    this.#status = status;
    this.#inputReportRingPushed = 0;
    this.#inputReportRingDropped = 0;
    this.#inputReportFallback = 0;
  }

  getInputReportRingStats(): WebHidInputReportRingStats {
    return {
      enabled: this.#inputReportRing !== null,
      pushed: this.#inputReportRingPushed,
      dropped: this.#inputReportRingDropped,
      fallback: this.#inputReportFallback,
    };
  }

  getLastInputReportInfo(device: HIDDevice): WebHidLastInputReportInfo | null {
    const deviceId = this.#deviceIdByDevice.get(device);
    if (deviceId === undefined) return null;
    return this.#lastInputReportInfo.get(deviceId) ?? null;
  }

  getOutputSendStats(): WebHidOutputSendStats {
    const deviceIds = new Set<number>();
    for (const deviceId of this.#pendingDeviceSends.keys()) deviceIds.add(deviceId);
    for (const deviceId of this.#outputSendDroppedByDevice.keys()) deviceIds.add(deviceId);

    const devices = Array.from(deviceIds)
      .sort((a, b) => a - b)
      .map((deviceId) => ({
        deviceId,
        pending: this.#pendingDeviceSends.get(deviceId)?.length ?? 0,
        pendingBytes: this.#pendingDeviceSendBytesByDevice.get(deviceId) ?? 0,
        dropped: this.#outputSendDroppedByDevice.get(deviceId) ?? 0,
      }));

    return {
      maxPendingDeviceSends: this.#maxPendingSendsPerDevice,
      maxPendingSendsPerDevice: this.#maxPendingSendsPerDevice,
      maxPendingSendBytesPerDevice: this.#maxPendingSendBytesPerDevice,
      maxPendingSendsTotal: Number.isFinite(this.#maxPendingSendsTotal) ? this.#maxPendingSendsTotal : null,
      pendingTotal: this.#pendingDeviceSendTotal,
      pendingBytesTotal: this.#pendingDeviceSendBytesTotal,
      droppedTotal: this.#outputSendDropped,
      devices,
    };
  }

  attachWorkerPort(port: MessagePort | Worker): void {
    if (this.#workerPort === port) return;

    // Replacing the worker is treated as a new guest session: previously-attached
    // devices must be explicitly re-attached by the user before the new worker is
    // allowed to access them.
    if (this.#workerPort) {
      this.detachWorkerPort(this.#workerPort);
    }

    this.#workerPort = port;

    const onMessage: EventListener = (ev) => {
      const data = (ev as MessageEvent<unknown>).data;
      if (isHidAttachResultMessage(data)) {
        this.#handleAttachResultMessage(port, data);
        return;
      }

      if (isHidSendReportMessage(data)) {
        this.#handleSendReportRequest(data);
        return;
      }

      if (isHidGetFeatureReportMessage(data)) {
        this.#handleGetFeatureReportRequest(data, port);
        return;
      }

      if (isHidRingDetachMessage(data)) {
        const reason = data.reason ?? "HID proxy rings disabled.";
        this.#handleRingFailure(reason, { notifyWorker: false });
        return;
      }

      if (isHidLogMessage(data)) {
        console.log(`[webhid] ${data.message}`);
        return;
      }

      if (isHidErrorMessage(data)) {
        console.warn(`[webhid] ${data.message}`);
        return;
      }
    };

    this.#workerPortListener = onMessage;
    port.addEventListener("message", onMessage);
    // When using addEventListener() MessagePorts need start() to begin dispatch.
    (port as unknown as { start?: () => void }).start?.();

    this.#maybeInitInputReportRing(port);
    this.#attachRings(port);
    this.#emit();
  }

  detachWorkerPort(port?: MessagePort | Worker): void {
    const active = this.#workerPort;
    if (!active) return;
    if (port && port !== active) return;

    for (const [deviceId, pending] of this.#pendingAttachResults) {
      if (pending.worker !== active) continue;
      this.#pendingAttachResults.delete(deviceId);
      if (pending.timeout) {
        clearTimeout(pending.timeout);
      }
      pending.reject(new Error("IO worker disconnected while attaching HID device."));
    }

    this.#detachRings();

    // Best-effort notify the worker that all devices are detached.
    for (const deviceId of this.#attachedToWorker) {
      const msg: HidDetachMessage = { type: "hid.detach", deviceId };
      try {
        active.postMessage(msg);
      } catch {
        // ignore
      }
    }

    // Remove input listeners so devices are no longer forwarded to a new worker
    // without an explicit user action.
    for (const deviceId of this.#attachedToWorker) {
      this.#unbridgeDevice(deviceId, { sendDetach: false });
    }
    this.#attachedToWorker.clear();

    if (this.#workerPortListener) {
      active.removeEventListener("message", this.#workerPortListener);
    }

    this.#workerPort = null;
    this.#workerPortListener = null;
    this.#inputReportRing = null;
    this.#emit();
  }

  #canUseSharedMemory(): boolean {
    // SharedArrayBuffer requires cross-origin isolation in browsers. Node/Vitest may still provide it,
    // but keep the check aligned with the browser contract so behaviour matches production.
    if ((globalThis as unknown as { crossOriginIsolated?: unknown }).crossOriginIsolated !== true) return false;
    if (typeof SharedArrayBuffer === "undefined") return false;
    if (typeof Atomics === "undefined") return false;
    return true;
  }

  #attachRings(worker: MessagePort | Worker): void {
    if (this.#inputRing && this.#outputRing) return;
    if (!this.#canUseSharedMemory()) return;

    const inputSab = createHidReportRingBuffer(LEGACY_HID_INPUT_RING_CAPACITY_BYTES);
    const outputSab = createHidReportRingBuffer(this.#outputRingCapacityBytes);
    this.#inputRing = new HidReportRing(inputSab);
    this.#outputRing = new HidReportRing(outputSab);
    this.#ringDetachSent = false;

    const msg: HidRingAttachMessage = { type: "hid.ringAttach", inputRing: inputSab, outputRing: outputSab };
    this.#postToWorker(worker, msg);

    // Drain output reports in the background. In Node (Vitest), `unref()` the timer so it doesn't
    // keep the test runner alive when a broker isn't explicitly destroyed.
    this.#outputRingDrainTimer = setInterval(() => this.#drainOutputRing(), 8);
    (this.#outputRingDrainTimer as unknown as { unref?: () => void }).unref?.();
  }

  #maybeInitInputReportRing(worker: MessagePort | Worker): void {
    // If the ring was explicitly configured by the caller, respect that.
    if (this.#inputReportRing) return;
    if (!this.#canUseSharedMemory()) return;

    const cap = this.#inputReportRingCapacityBytes;
    const sab = new SharedArrayBuffer(ringCtrl.BYTES + cap);
    new Int32Array(sab, 0, ringCtrl.WORDS).set([0, 0, 0, cap]);
    this.#inputReportRing = new RingBuffer(sab, 0);
    this.#inputReportRingPushed = 0;
    this.#inputReportRingDropped = 0;
    this.#inputReportFallback = 0;

    const msg: HidRingInitMessage = { type: "hid.ring.init", sab, offsetBytes: 0 };
    this.#postToWorker(worker, msg);
  }

  #detachRings(): void {
    if (this.#outputRingDrainTimer) {
      clearInterval(this.#outputRingDrainTimer);
      this.#outputRingDrainTimer = null;
    }
    this.#inputRing = null;
    this.#outputRing = null;
  }

  #warnOutputSendDrop(deviceId: number): void {
    const now = typeof performance !== "undefined" ? performance.now() : Date.now();
    const lastWarn = this.#outputSendDropWarnedAtByDevice.get(deviceId);
    if (lastWarn !== undefined && now - lastWarn < OUTPUT_SEND_DROP_WARN_INTERVAL_MS) return;
    this.#outputSendDropWarnedAtByDevice.set(deviceId, now);

    const dropped = this.#outputSendDroppedByDevice.get(deviceId) ?? 0;
    const pending = this.#pendingDeviceSends.get(deviceId)?.length ?? 0;
    const pendingBytes = this.#pendingDeviceSendBytesByDevice.get(deviceId) ?? 0;
    console.warn(
      `[webhid] Dropping queued HID report tasks for deviceId=${deviceId} (pending=${pending} pendingBytes=${pendingBytes} maxPendingDeviceSends=${this.#maxPendingSendsPerDevice} maxPendingSendBytesPerDevice=${this.#maxPendingSendBytesPerDevice} dropped=${dropped})`,
    );
  }

  #recordOutputSendDrop(deviceId: number): void {
    this.#outputSendDropped += 1;
    this.#outputSendDroppedByDevice.set(deviceId, (this.#outputSendDroppedByDevice.get(deviceId) ?? 0) + 1);
    const status = this.#status;
    if (status) {
      try {
        Atomics.add(status, StatusIndex.IoHidOutputReportDropCounter, 1);
      } catch {
        // ignore (status may not be SharedArrayBuffer-backed in tests/harnesses)
      }
    }
    this.#warnOutputSendDrop(deviceId);
  }

  #enqueueDeviceSend(deviceId: number, taskBytes: number, createTask: HidDeviceSendTaskFactory): boolean {
    // Normalise and sanity-check byte accounting; callers should pass the amount of memory the
    // queued task will retain (e.g. the report payload length).
    const bytes = (() => {
      if (!Number.isFinite(taskBytes) || taskBytes < 0) return 0;
      return Math.floor(taskBytes) >>> 0;
    })();
    const queueLen = this.#pendingDeviceSends.get(deviceId)?.length ?? 0;
    const queueBytes = this.#pendingDeviceSendBytesByDevice.get(deviceId) ?? 0;
    // Drop policy: drop newest when at/over the cap. This preserves FIFO ordering for already-queued
    // reports and keeps memory bounded when the guest spams reports or `sendReport()` hangs.
    if (
      queueLen >= this.#maxPendingSendsPerDevice ||
      this.#pendingDeviceSendTotal >= this.#maxPendingSendsTotal ||
      bytes > this.#maxPendingSendBytesPerDevice ||
      queueBytes + bytes > this.#maxPendingSendBytesPerDevice
    ) {
      this.#recordOutputSendDrop(deviceId);
      return false;
    }

    let queue = this.#pendingDeviceSends.get(deviceId);
    if (!queue) {
      queue = [];
      this.#pendingDeviceSends.set(deviceId, queue);
    }
    // Create the task only after we know it will be queued so we don't eagerly copy payload
    // buffers (e.g. SharedArrayBuffer-backed ring payloads) just to immediately drop them.
    queue.push({ bytes, run: createTask() });
    this.#pendingDeviceSendTotal += 1;
    this.#pendingDeviceSendBytesTotal += bytes;
    this.#pendingDeviceSendBytesByDevice.set(deviceId, queueBytes + bytes);
    if (this.#deviceSendTokenById.has(deviceId)) return true;
    const token = this.#nextDeviceSendToken++;
    this.#deviceSendTokenById.set(deviceId, token);
    void this.#runDeviceSendQueue(deviceId, token);
    return true;
  }

  #dequeueDeviceSend(deviceId: number): HidDeviceSendTask | null {
    const queue = this.#pendingDeviceSends.get(deviceId);
    if (!queue || queue.length === 0) return null;
    const task = queue.shift()!;
    this.#pendingDeviceSendTotal -= 1;
    this.#pendingDeviceSendBytesTotal -= task.bytes;
    const nextBytes = (this.#pendingDeviceSendBytesByDevice.get(deviceId) ?? 0) - task.bytes;
    if (nextBytes > 0) {
      this.#pendingDeviceSendBytesByDevice.set(deviceId, nextBytes);
    } else {
      this.#pendingDeviceSendBytesByDevice.delete(deviceId);
    }
    if (queue.length === 0) {
      // Avoid leaking per-device arrays once all work is drained.
      this.#pendingDeviceSends.delete(deviceId);
      this.#pendingDeviceSendBytesByDevice.delete(deviceId);
    }
    return task.run;
  }

  async #runDeviceSendQueue(deviceId: number, token: number): Promise<void> {
    try {
      // eslint-disable-next-line no-constant-condition
      while (true) {
        if (this.#deviceSendTokenById.get(deviceId) !== token) break;
        // Intentionally fetch the next task via a helper so the queue array is not retained across
        // `await` points. This allows `detach` to drop pending tasks and release memory even if an
        // in-flight `sendReport` Promise never resolves.
        const task = this.#dequeueDeviceSend(deviceId);
        if (!task) break;
        try {
          await task();
        } catch (err) {
          // Individual tasks should already handle/report errors, but ensure we never stop draining.
          const message = formatOneLineError(err, 512);
          console.warn(`[webhid] Unhandled HID send task error deviceId=${deviceId}: ${message}`);
        }
      }
    } finally {
      if (this.#deviceSendTokenById.get(deviceId) === token) {
        this.#deviceSendTokenById.delete(deviceId);
      }
    }
  }

  #handleRingFailure(reason: string, options: { notifyWorker?: boolean } = {}): void {
    // Avoid spamming `hid.ringDetach` if multiple callbacks notice the failure.
    if (!this.#inputRing && !this.#outputRing && !this.#outputRingDrainTimer && !this.#inputReportRing) return;

    this.#detachRings();
    // Disable the input-report SharedArrayBuffer ring as well so we fully fall back to postMessage.
    // The SAB rings are an optimization and may be disabled at any time (e.g. on corruption).
    this.#inputReportRing = null;
    this.#status = null;

    const shouldNotify = options.notifyWorker !== false;
    const worker = this.#workerPort;
    if (!shouldNotify || !worker) return;
    if (this.#ringDetachSent) return;
    this.#ringDetachSent = true;
    const msg: HidRingDetachMessage = { type: "hid.ringDetach", reason };
    this.#postToWorker(worker, msg);
  }

  #drainOutputRing(options: { stopAtTail?: number } = {}): void {
    const ring = this.#outputRing;
    if (!ring) return;

    const stopAtTail = options.stopAtTail;
    let remainingRecords = stopAtTail === undefined ? MAX_HID_OUTPUT_RING_RECORDS_PER_DRAIN_TICK : Number.POSITIVE_INFINITY;
    let remainingBytes = stopAtTail === undefined ? MAX_HID_OUTPUT_RING_BYTES_PER_DRAIN_TICK : Number.POSITIVE_INFINITY;

    try {
      while (remainingRecords > 0 && remainingBytes > 0) {
        // When draining to enforce ordering relative to an immediate `hid.sendReport` fallback,
        // do not spin forever if the worker keeps writing to the ring. Instead, snapshot the
        // ring's `tail` and stop once the consumer `head` reaches that value, leaving any
        // later writes for the next drain tick.
        if (stopAtTail !== undefined) {
          const { head } = ring.debugState();
          if (head === stopAtTail) break;
        }

        let payloadLen = 0;
        const ok = ring.consumeNextOrThrow((rec) => {
          payloadLen = rec.payload.byteLength;
          if (rec.reportType !== HidRingReportType.Output && rec.reportType !== HidRingReportType.Feature) return;

          const deviceId = rec.deviceId >>> 0;
          const pendingAttach = this.#pendingAttachResults.get(deviceId);
          if (!this.#attachedToWorker.has(deviceId) && !pendingAttach) return;

          // If the device is still attaching, defer execution until the worker confirms via
          // `hid.attachResult`. This avoids dropping output reports produced during the attach
          // handshake while still ensuring we don't send reports for failed attaches.
          const attachPromise = pendingAttach?.promise;
          const payload = rec.payload;
          const reportType = rec.reportType === HidRingReportType.Feature ? "feature" : "output";
          const reportId = rec.reportId >>> 0;
          const srcLen = payload.byteLength;
          const expected = (reportType === "feature"
            ? this.#featureReportExpectedPayloadBytes.get(deviceId)?.get(reportId)
            : this.#outputReportExpectedPayloadBytes.get(deviceId)?.get(reportId)) as number | undefined;

          let destLen: number;
          let warnKind: "truncated" | "padded" | "hardCap" | null = null;
          let warnMessage = "";
          if (expected !== undefined) {
            destLen = expected;
            if (srcLen > expected) {
              warnKind = "truncated";
              warnMessage = `[webhid] ${reportType === "feature" ? "sendFeatureReport" : "sendReport"} length mismatch (deviceId=${deviceId} reportId=${reportId} expected=${expected} got=${srcLen}); truncating`;
            } else if (srcLen < expected) {
              warnKind = "padded";
              warnMessage = `[webhid] ${reportType === "feature" ? "sendFeatureReport" : "sendReport"} length mismatch (deviceId=${deviceId} reportId=${reportId} expected=${expected} got=${srcLen}); zero-padding`;
            }
          } else {
            const hardCap = maxHidControlPayloadBytes(reportId);
            if (srcLen > hardCap) {
              destLen = hardCap;
              warnKind = "hardCap";
              warnMessage = `[webhid] ${reportType === "feature" ? "sendFeatureReport" : "sendReport"} reportId=${reportId} for deviceId=${deviceId} has unknown expected size; capping ${srcLen} bytes to ${hardCap}`;
            } else {
              destLen = srcLen;
            }
          }

          const queued = this.#enqueueDeviceSend(deviceId, destLen, () => {
            if (warnKind) {
              this.#warnSendReportSizeOnce(deviceId, reportType, reportId, warnKind, warnMessage);
            }

            // Copy the report payload out of the ring immediately so it can't be overwritten by
            // subsequent producer writes while we are awaiting an in-flight WebHID send.
            const copyLen = Math.min(srcLen, destLen);
            const src = payload.subarray(0, copyLen);
            const data = new Uint8Array(destLen);
            data.set(src);
            return async () => {
              if (attachPromise) {
                try {
                  await attachPromise;
                } catch {
                  return;
                }
              }
              const device = this.#deviceById.get(deviceId);
              if (!device) return;
              try {
                if (reportType === "output") {
                  await device.sendReport(reportId, data);
                } else {
                  await device.sendFeatureReport(reportId, data);
                }
              } catch (err) {
                const message = formatOneLineError(err, 512);
                console.warn(`[webhid] Failed to send ${reportType} reportId=${reportId} deviceId=${deviceId}: ${message}`);
              }
            };
          });

          if (queued) {
            // Approximate transient allocation cost (we allocate `destLen` bytes when queueing).
            payloadLen = destLen;
          }
        });
        if (!ok) break;
        remainingRecords -= 1;
        remainingBytes -= payloadLen;
      }
    } catch (err) {
      const message = formatOneLineError(err, 512);
      this.#handleRingFailure(`HID proxy rings disabled: ${message}`);
    }
  }

  getDeviceId(device: HIDDevice): number {
    const existing = this.#deviceIdByDevice.get(device);
    if (existing !== undefined) return existing;
    const id = this.#nextDeviceId++;
    this.#deviceIdByDevice.set(device, id);
    this.#deviceById.set(id, device);
    return id;
  }

  isAttachedToWorker(device: HIDDevice): boolean {
    const id = this.#deviceIdByDevice.get(device);
    if (id === undefined) return false;
    return this.#attachedToWorker.has(id);
  }

  async attachDevice(device: HIDDevice): Promise<number> {
    const worker = this.#workerPort;
    if (!worker) throw new Error("IO worker is not attached; start the VM workers first.");

    const deviceId = this.getDeviceId(device);
    if (this.#attachedToWorker.has(deviceId)) return deviceId;

    let sentAttachToWorker = false;
    try {
      await this.manager.attachKnownDevice(device);

      // `attachKnownDevice()` can be followed by a concurrent detach/disconnect (user action or
      // physical unplug) before we reach the worker handshake. Bail out early in that case so we
      // don't attach a device the manager already considers detached.
      if (this.#deviceIdByDevice.get(device) !== deviceId) {
        throw new Error("HID device was detached while attaching.");
      }

      const guestPathHint: GuestUsbPath | undefined = this.manager
        .getState()
        .attachedDevices.find((entry) => entry.device === device)?.guestPath;
      const guestPortHint = guestPathHint?.[0];

      // The WebHID `@types/w3c-web-hid` definitions mark many collection fields as optional,
      // but real Chromium devices always populate them. `normalizeCollections` expects a
      // fully-populated shape matching the Rust contract, so cast and let the normalizer
      // throw if a browser provides incomplete metadata.
      //
      // Validate key invariants here (mixed report IDs, out-of-order isRange bounds, etc.) so we
      // fail deterministically before sending metadata to the worker.
      const collections = normalizeCollections(device.collections, { validate: true });
      const hasInterruptOut = computeHasInterruptOut(collections);
      const inputReportPayloadBytes = computeInputReportPayloadByteLengths(collections);
      this.#inputReportExpectedPayloadBytes.set(deviceId, inputReportPayloadBytes);
      const featureReportPayloadBytes = computeFeatureReportPayloadByteLengths(collections);
      this.#featureReportExpectedPayloadBytes.set(deviceId, featureReportPayloadBytes);
      const outputReportPayloadBytes = computeOutputReportPayloadByteLengths(collections);
      this.#outputReportExpectedPayloadBytes.set(deviceId, outputReportPayloadBytes);

      const attachMsg: HidAttachMessage = {
        type: "hid.attach",
        deviceId,
        vendorId: device.vendorId,
        productId: device.productId,
        ...(device.productName ? { productName: device.productName } : {}),
        ...(guestPathHint ? { guestPath: guestPathHint } : {}),
        ...(guestPortHint === 0 || guestPortHint === 1 ? { guestPort: guestPortHint } : {}),
        collections,
        hasInterruptOut,
      };

      if (this.#workerPort !== worker) {
        throw new Error("IO worker disconnected while attaching HID device.");
      }

      // The user might detach the HID device while we were preparing the attach message (descriptor
      // normalization etc). Ensure we're still attaching the same session.
      if (this.#deviceIdByDevice.get(device) !== deviceId) {
        throw new Error("HID device was detached while attaching.");
      }

      const attachResult = this.#waitForAttachResult(worker, deviceId);
      sentAttachToWorker = true;
      this.#postToWorker(worker, attachMsg);

      await attachResult;

      const onInputReport = (event: HIDInputReportEvent): void => {
        const activeWorker = this.#workerPort;
        if (!activeWorker) return;
        if (!this.#attachedToWorker.has(deviceId)) return;

        const view = event.data;
        if (!(view instanceof DataView)) return;
        const rawReportId = (event as unknown as { reportId?: unknown }).reportId;
        if (
          rawReportId !== undefined &&
          (typeof rawReportId !== "number" || !Number.isInteger(rawReportId) || rawReportId < 0 || rawReportId > 0xff)
        ) {
          this.#warnInputReportSizeOnce(
            deviceId,
            0,
            "invalidReportId",
            `[webhid] inputreport has invalid reportId=${String(rawReportId)} for deviceId=${deviceId}; dropping`,
          );
          return;
        }
        const reportId = (rawReportId === undefined ? 0 : rawReportId) >>> 0;
        const srcLen = view.byteLength;

        const expected = this.#inputReportExpectedPayloadBytes.get(deviceId)?.get(reportId);
        let destLen: number;
        if (expected !== undefined) {
          destLen = expected;
          if (srcLen > expected) {
            this.#inputReportTruncated += 1;
            this.#warnInputReportSizeOnce(
              deviceId,
              reportId,
              "truncated",
              `[webhid] inputreport length mismatch (deviceId=${deviceId} reportId=${reportId} expected=${expected} got=${srcLen}); truncating`,
            );
          } else if (srcLen < expected) {
            this.#inputReportPadded += 1;
            this.#warnInputReportSizeOnce(
              deviceId,
              reportId,
              "padded",
              `[webhid] inputreport length mismatch (deviceId=${deviceId} reportId=${reportId} expected=${expected} got=${srcLen}); zero-padding`,
            );
          }
        } else {
          this.#inputReportUnknownSize += 1;
          if (srcLen > UNKNOWN_INPUT_REPORT_HARD_CAP_BYTES) {
            destLen = UNKNOWN_INPUT_REPORT_HARD_CAP_BYTES;
            this.#inputReportHardCapped += 1;
            this.#warnInputReportSizeOnce(
              deviceId,
              reportId,
              "hardCap",
              `[webhid] inputreport reportId=${reportId} for deviceId=${deviceId} has unknown expected size; capping ${srcLen} bytes to ${UNKNOWN_INPUT_REPORT_HARD_CAP_BYTES}`,
            );
          } else {
            destLen = srcLen;
          }
        }

        // Only ever create a view over the clamped amount of feature data so a bogus
        // (or malicious) browser/device can't trick us into copying huge buffers.
        const copyLen = Math.min(srcLen, destLen);
        const src = new Uint8Array(view.buffer, view.byteOffset, copyLen);

        const tsMs = typeof event.timeStamp === "number" ? event.timeStamp : undefined;
        this.#lastInputReportInfo.set(deviceId, { tsMs: tsMs ?? performance.now(), byteLength: srcLen });
        this.#scheduleEmitForInputReports();

        const ring = this.#inputReportRing;
        if (ring && this.#canUseSharedMemory()) {
          const tsU32 = toU32OrZero(tsMs);
          let ok = false;
          try {
            ok = ring.tryPushWithWriterSpsc(HID_INPUT_REPORT_RECORD_HEADER_BYTES + destLen, (dest) => {
              const dv = new DataView(dest.buffer, dest.byteOffset, dest.byteLength);
              dv.setUint32(0, HID_INPUT_REPORT_RECORD_MAGIC, true);
              dv.setUint32(4, HID_INPUT_REPORT_RECORD_VERSION, true);
              dv.setUint32(8, deviceId >>> 0, true);
              dv.setUint32(12, reportId, true);
              dv.setUint32(16, tsU32, true);
              dv.setUint32(20, destLen >>> 0, true);
              const payload = dest.subarray(HID_INPUT_REPORT_RECORD_HEADER_BYTES);
              payload.set(src);
              if (copyLen < destLen) payload.fill(0, copyLen);
            });
          } catch (err) {
            const message = formatOneLineError(err, 512);
            this.#handleRingFailure(`HID proxy rings disabled: ${message}`);
          }
          if (ok && this.#inputReportRing === ring) {
            this.#inputReportRingPushed += 1;
            return;
          }
          if (this.#inputReportRing === ring) {
            // Drop rather than blocking/spinning; this is a best-effort fast path.
            this.#inputReportRingDropped += 1;
            const status = this.#status;
            if (status) {
              try {
                Atomics.add(status, StatusIndex.IoHidInputReportDropCounter, 1);
              } catch {
                // ignore (status may not be SharedArrayBuffer-backed in tests/harnesses)
              }
            }
            return;
          }
          // Ring was detached due to corruption; fall through to postMessage.
        }

        const inputRing = this.#inputRing;
        if (inputRing) {
          if (copyLen === destLen) {
            inputRing.push(deviceId >>> 0, HidRingReportType.Input, reportId, src);
          } else {
            const padded = new Uint8Array(destLen);
            padded.set(src);
            inputRing.push(deviceId >>> 0, HidRingReportType.Input, reportId, padded);
          }
          return;
        }

        const data = new Uint8Array(destLen);
        data.set(src);
        const msg: HidInputReportMessage = {
          type: "hid.inputReport",
          deviceId,
          reportId,
          data,
          tsMs,
        };
        this.#inputReportFallback += 1;
        this.#postToWorker(activeWorker, msg, [data.buffer]);
      };

      device.addEventListener("inputreport", onInputReport);
      this.#inputReportListeners.set(deviceId, onInputReport);
      this.#attachedToWorker.add(deviceId);
      this.#emit();

      return deviceId;
    } catch (err) {
      if (sentAttachToWorker) {
        try {
          worker.postMessage({ type: "hid.detach", deviceId } satisfies HidDetachMessage);
        } catch {
          // ignore
        }
      }

      // Ensure we don't leak manager-side guest paths / open handles when attaching fails.
      try {
        this.#unbridgeDevice(deviceId, { sendDetach: false });
      } catch {
        // ignore
      }
      this.#attachedToWorker.delete(deviceId);
      this.#emit();
      await this.manager.detachDevice(device).catch(() => undefined);

      throw err;
    }
  }

  #cancelPendingAttach(deviceId: number, err: Error): void {
    const pending = this.#pendingAttachResults.get(deviceId);
    if (!pending) return;
    this.#pendingAttachResults.delete(deviceId);
    if (pending.timeout) {
      clearTimeout(pending.timeout);
    }
    pending.reject(err);
  }

  #waitForAttachResult(worker: MessagePort | Worker, deviceId: number): Promise<void> {
    const existing = this.#pendingAttachResults.get(deviceId);
    if (existing) return existing.promise;

    let resolve!: () => void;
    let reject!: (err: Error) => void;
    const promise = new Promise<void>((res, rej) => {
      resolve = res;
      reject = (err: Error) => rej(err);
    });

    const entry = { worker, promise, resolve, reject, timeout: null as ReturnType<typeof setTimeout> | null };
    this.#pendingAttachResults.set(deviceId, entry);

    const timeoutMs = this.#attachResultTimeoutMs;
    entry.timeout = setTimeout(() => {
      const pending = this.#pendingAttachResults.get(deviceId);
      if (!pending || pending.promise !== promise) return;
      this.#pendingAttachResults.delete(deviceId);
      pending.reject(new Error(`[webhid] Timed out waiting for hid.attachResult deviceId=${deviceId}`));
    }, timeoutMs);
    // In Node (Vitest), `unref()` the timer so it doesn't keep the test runner alive if
    // the broker is not explicitly destroyed.
    (entry.timeout as unknown as { unref?: () => void }).unref?.();

    return promise;
  }

  #handleAttachResultMessage(port: MessagePort | Worker, msg: HidAttachResultMessage): void {
    const pending = this.#pendingAttachResults.get(msg.deviceId);
    if (!pending || pending.worker !== port) return;
    this.#pendingAttachResults.delete(msg.deviceId);
    if (pending.timeout) {
      clearTimeout(pending.timeout);
    }
    if (msg.ok) {
      pending.resolve();
    } else {
      pending.reject(new Error(msg.error ?? `Failed to attach HID deviceId=${msg.deviceId} on IO worker.`));
    }
  }

  async detachDevice(device: HIDDevice): Promise<void> {
    const deviceId = this.#deviceIdByDevice.get(device);
    if (deviceId !== undefined) {
      this.#cancelPendingAttach(deviceId, new Error("HID device was detached while waiting for hid.attachResult."));
      this.#unbridgeDevice(deviceId, { sendDetach: true });
      this.#attachedToWorker.delete(deviceId);
      // Fully forget this device so the HIDDevice object can be garbage-collected.
      // Device IDs are monotonic and do not need to be re-used.
      this.#deviceIdByDevice.delete(device);
      this.#deviceById.delete(deviceId);
      this.#emit();
    }

    await this.manager.detachDevice(device);
  }

  async #handleManagerDeviceDetached(device: HIDDevice): Promise<void> {
    const deviceId = this.#deviceIdByDevice.get(device);
    if (deviceId === undefined) return;

    this.#cancelPendingAttach(deviceId, new Error("HID device disconnected while waiting for hid.attachResult."));
    const sendDetach = this.#attachedToWorker.has(deviceId);
    this.#unbridgeDevice(deviceId, { sendDetach });
    this.#attachedToWorker.delete(deviceId);
    // The manager already considers the device detached, so release our references.
    this.#deviceIdByDevice.delete(device);
    this.#deviceById.delete(deviceId);
    this.#emit();
  }

  #unbridgeDevice(deviceId: number, options: { sendDetach: boolean }): void {
    const device = this.#deviceById.get(deviceId);
    const listener = this.#inputReportListeners.get(deviceId);
    if (device && listener) {
      try {
        device.removeEventListener("inputreport", listener);
      } catch {
        // ignore
      }
    }
    this.#inputReportListeners.delete(deviceId);
    this.#lastInputReportInfo.delete(deviceId);
    this.#inputReportExpectedPayloadBytes.delete(deviceId);
    this.#featureReportExpectedPayloadBytes.delete(deviceId);
    this.#outputReportExpectedPayloadBytes.delete(deviceId);
    const pendingAttach = this.#pendingAttachResults.get(deviceId);
    if (pendingAttach) {
      this.#pendingAttachResults.delete(deviceId);
      if (pendingAttach.timeout) {
        clearTimeout(pendingAttach.timeout);
      }
      pendingAttach.reject(new Error(`[webhid] HID deviceId=${deviceId} detached while waiting for hid.attachResult`));
    }
    // Allow future attaches to re-log size mismatches for this device ID.
    for (const key of this.#inputReportSizeWarned) {
      if (key.startsWith(`${deviceId}:`)) {
        this.#inputReportSizeWarned.delete(key);
      }
    }
    for (const key of this.#featureReportSizeWarned) {
      if (key.startsWith(`${deviceId}:`)) {
        this.#featureReportSizeWarned.delete(key);
      }
    }
    for (const key of this.#sendReportSizeWarned) {
      if (key.startsWith(`${deviceId}:`)) {
        this.#sendReportSizeWarned.delete(key);
      }
    }

    const pending = this.#pendingDeviceSends.get(deviceId);
    if (pending) {
      this.#pendingDeviceSendTotal -= pending.length;
      const pendingBytes = this.#pendingDeviceSendBytesByDevice.get(deviceId) ?? 0;
      this.#pendingDeviceSendBytesTotal -= pendingBytes;
      this.#pendingDeviceSends.delete(deviceId);
      this.#pendingDeviceSendBytesByDevice.delete(deviceId);
    }
    this.#deviceSendTokenById.delete(deviceId);
    this.#outputSendDroppedByDevice.delete(deviceId);
    this.#outputSendDropWarnedAtByDevice.delete(deviceId);

    if (options.sendDetach && this.#workerPort) {
      const detachMsg: HidDetachMessage = { type: "hid.detach", deviceId };
      this.#postToWorker(this.#workerPort, detachMsg);
    }
  }

  #scheduleEmitForInputReports(): void {
    if (this.#listeners.size === 0) return;
    if (this.#inputReportEmitTimer) return;
    this.#inputReportEmitTimer = setTimeout(() => {
      this.#inputReportEmitTimer = null;
      this.#emit();
    }, 100);
    (this.#inputReportEmitTimer as unknown as { unref?: () => void }).unref?.();
  }

  #warnInputReportSizeOnce(deviceId: number, reportId: number, kind: string, message: string): void {
    const key = `${deviceId}:${reportId}:${kind}`;
    if (this.#inputReportSizeWarned.has(key)) return;
    this.#inputReportSizeWarned.add(key);
    console.warn(message);
  }

  #warnFeatureReportSizeOnce(deviceId: number, reportId: number, kind: string, message: string): void {
    const key = `${deviceId}:${reportId}:${kind}`;
    if (this.#featureReportSizeWarned.has(key)) return;
    this.#featureReportSizeWarned.add(key);
    console.warn(message);
  }

  #warnSendReportSizeOnce(deviceId: number, reportType: "output" | "feature", reportId: number, kind: string, message: string): void {
    const key = `${deviceId}:${reportType}:${reportId}:${kind}`;
    if (this.#sendReportSizeWarned.has(key)) return;
    this.#sendReportSizeWarned.add(key);
    console.warn(message);
  }

  #handleSendReportRequest(msg: HidSendReportMessage): void {
    // The worker prefers the SharedArrayBuffer output ring, but can fall back to structured
    // `hid.sendReport` messages when the ring is full or the payload is too large. Because the
    // ring is drained on a timer, a fallback message could otherwise overtake earlier ring
    // records for the same device. Drain pending ring records *synchronously* before enqueuing
    // this message so the per-device send FIFO preserves guest ordering.
    const ring = this.#outputRing;
    if (ring) {
      const stopAtTail = (() => {
        const { head, tail, used } = ring.debugState();
        const stop = msg.outputRingTail;
        if (stop === undefined) return tail;
        const dist = ((stop >>> 0) - head) >>> 0;
        // Only drain towards `stop` when it lies within the current [head, tail] window.
        // If the periodic drain loop has already consumed past this tail snapshot, draining further
        // would only make ordering worse by pulling in even newer ring records ahead of this message.
        if (dist <= used) return stop >>> 0;
        return head;
      })();
      this.#drainOutputRing({ stopAtTail });
    }

    const deviceId = msg.deviceId >>> 0;
    const pendingAttach = this.#pendingAttachResults.get(deviceId);
    if (!this.#attachedToWorker.has(deviceId) && !pendingAttach) {
      console.warn(`[webhid] sendReport for detached deviceId=${deviceId}`);
      return;
    }

    if (!this.#deviceById.has(deviceId)) {
      console.warn(`[webhid] sendReport for unknown deviceId=${deviceId}`);
      return;
    }

    const attachPromise = pendingAttach?.promise;
    const reportType = msg.reportType;
    const reportId = msg.reportId >>> 0;
    const payload = msg.data;
    const srcLen = payload.byteLength;
    const expected = (reportType === "feature"
      ? this.#featureReportExpectedPayloadBytes.get(deviceId)?.get(reportId)
      : this.#outputReportExpectedPayloadBytes.get(deviceId)?.get(reportId)) as number | undefined;
    const hardCap = expected === undefined ? maxHidControlPayloadBytes(reportId) : 0;
    const destLen = expected === undefined ? Math.min(srcLen, hardCap) : expected;

    this.#enqueueDeviceSend(deviceId, destLen, () => {
      let data: Uint8Array<ArrayBuffer>;
      if (expected !== undefined) {
        if (srcLen === expected) {
          data = ensureArrayBufferBacked(payload);
        } else {
          if (srcLen > expected) {
            this.#warnSendReportSizeOnce(
              deviceId,
              reportType,
              reportId,
              "truncated",
              `[webhid] ${reportType === "feature" ? "sendFeatureReport" : "sendReport"} length mismatch (deviceId=${deviceId} reportId=${reportId} expected=${expected} got=${srcLen}); truncating`,
            );
          } else {
            this.#warnSendReportSizeOnce(
              deviceId,
              reportType,
              reportId,
              "padded",
              `[webhid] ${reportType === "feature" ? "sendFeatureReport" : "sendReport"} length mismatch (deviceId=${deviceId} reportId=${reportId} expected=${expected} got=${srcLen}); zero-padding`,
            );
          }
          const copyLen = Math.min(srcLen, expected);
          const src = payload.subarray(0, copyLen);
          const out = new Uint8Array(expected);
          out.set(src);
          data = out as Uint8Array<ArrayBuffer>;
        }
      } else if (srcLen > hardCap) {
        this.#warnSendReportSizeOnce(
          deviceId,
          reportType,
          reportId,
          "hardCap",
          `[webhid] ${reportType === "feature" ? "sendFeatureReport" : "sendReport"} reportId=${reportId} for deviceId=${deviceId} has unknown expected size; capping ${srcLen} bytes to ${hardCap}`,
        );
        const src = payload.subarray(0, hardCap);
        const out = new Uint8Array(hardCap);
        out.set(src);
        data = out as Uint8Array<ArrayBuffer>;
      } else {
        data = ensureArrayBufferBacked(payload);
      }

      return async () => {
        if (attachPromise) {
          try {
            await attachPromise;
          } catch {
            return;
          }
        }
        const device = this.#deviceById.get(deviceId);
        if (!device) return;
        try {
          if (reportType === "output") {
            await device.sendReport(reportId, data);
          } else {
            await device.sendFeatureReport(reportId, data);
          }
        } catch (err) {
          const message = formatOneLineError(err, 512);
          console.warn(`[webhid] Failed to send ${reportType} reportId=${reportId} deviceId=${deviceId}: ${message}`);
        }
      };
    });
  }

  #handleGetFeatureReportRequest(msg: HidGetFeatureReportMessage, worker: MessagePort | Worker): void {
    const deviceId = msg.deviceId >>> 0;
    const reportId = msg.reportId >>> 0;

    // Feature-report reads must be ordered relative to any output/feature report writes that may be
    // pending in the SAB output ring. Drain already-produced ring records so this read cannot
    // overtake them (even if the periodic ring drain timer hasn't run yet).
    const ring = this.#outputRing;
    if (ring) {
      const stopAtTail = (() => {
        const { head, tail, used } = ring.debugState();
        const stop = msg.outputRingTail;
        if (stop === undefined) return tail;
        const dist = ((stop >>> 0) - head) >>> 0;
        if (dist <= used) return stop >>> 0;
        return head;
      })();
      this.#drainOutputRing({ stopAtTail });
    }

    const base = {
      type: "hid.featureReportResult" as const,
      requestId: msg.requestId,
      deviceId,
      reportId,
    };

    // If the device is not attached (and not in the middle of attaching), respond immediately
    // rather than queueing work. This keeps memory bounded even if a worker sends feature report
    // requests for stale/unknown deviceIds.
    const pendingAttach = this.#pendingAttachResults.get(deviceId);
    if (!this.#attachedToWorker.has(deviceId) && !pendingAttach) {
      const error = this.#deviceById.has(deviceId) ? `DeviceId=${deviceId} is not attached.` : `Unknown deviceId=${deviceId}.`;
      const res: HidFeatureReportResultMessage = { ...base, ok: false, error };
      this.#postToWorker(worker, res);
      return;
    }

    const attachPromise = pendingAttach?.promise;
    // Use the same per-device FIFO as output/feature report sends so receiveFeatureReport
    // requests are serialized relative to any queued report I/O for that device.
    const ok = this.#enqueueDeviceSend(deviceId, 0, () => async () => {
      if (attachPromise) {
        try {
          await attachPromise;
        } catch (err) {
          const message = formatOneLineError(err, 512);
          const res: HidFeatureReportResultMessage = { ...base, ok: false, error: message };
          this.#postToWorker(worker, res);
          return;
        }
      }

      if (!this.#attachedToWorker.has(deviceId)) {
        const res: HidFeatureReportResultMessage = { ...base, ok: false, error: `DeviceId=${deviceId} is not attached.` };
        this.#postToWorker(worker, res);
        return;
      }

      const device = this.#deviceById.get(deviceId);
      if (!device) {
        const res: HidFeatureReportResultMessage = { ...base, ok: false, error: `Unknown deviceId=${deviceId}.` };
        this.#postToWorker(worker, res);
        return;
      }

      try {
        const view = await device.receiveFeatureReport(reportId);
        if (!(view instanceof DataView)) {
          const res: HidFeatureReportResultMessage = { ...base, ok: false, error: "receiveFeatureReport returned non-DataView value." };
          this.#postToWorker(worker, res);
          return;
        }

        const srcLen = view.byteLength;
        const expected = this.#featureReportExpectedPayloadBytes.get(deviceId)?.get(reportId);
        let destLen: number;
        if (expected !== undefined) {
          destLen = expected;
          if (srcLen > expected) {
            this.#warnFeatureReportSizeOnce(
              deviceId,
              reportId,
              "truncated",
              `[webhid] feature report length mismatch (deviceId=${deviceId} reportId=${reportId} expected=${expected} got=${srcLen}); truncating`,
            );
          } else if (srcLen < expected) {
            this.#warnFeatureReportSizeOnce(
              deviceId,
              reportId,
              "padded",
              `[webhid] feature report length mismatch (deviceId=${deviceId} reportId=${reportId} expected=${expected} got=${srcLen}); zero-padding`,
            );
          }
        } else if (srcLen > UNKNOWN_FEATURE_REPORT_HARD_CAP_BYTES) {
          destLen = UNKNOWN_FEATURE_REPORT_HARD_CAP_BYTES;
          this.#warnFeatureReportSizeOnce(
            deviceId,
            reportId,
            "hardCap",
            `[webhid] feature report reportId=${reportId} for deviceId=${deviceId} has unknown expected size; capping ${srcLen} bytes to ${UNKNOWN_FEATURE_REPORT_HARD_CAP_BYTES}`,
          );
        } else {
          destLen = srcLen;
        }

        // Only ever create a view over the clamped amount of input data so a bogus
        // (or malicious) browser/device can't trick us into copying huge buffers.
        const copyLen = Math.min(srcLen, destLen);
        const src = new Uint8Array(view.buffer, view.byteOffset, copyLen);

        // Always send an ArrayBuffer-backed Uint8Array (transferable).
        const data = new Uint8Array(destLen);
        data.set(src);
        const res: HidFeatureReportResultMessage = { ...base, ok: true, data };
        this.#postToWorker(worker, res, [data.buffer]);
      } catch (err) {
        const message = formatOneLineError(err, 512);
        const res: HidFeatureReportResultMessage = { ...base, ok: false, error: message };
        this.#postToWorker(worker, res);
      }
    });
    if (!ok) {
      const res: HidFeatureReportResultMessage = { ...base, ok: false, error: "Too many pending HID report tasks for this device." };
      this.#postToWorker(worker, res);
    }
  }

  #postToWorker(worker: MessagePort | Worker, msg: HidProxyMessage, transfer?: Transferable[]): void {
    try {
      if (transfer) {
        worker.postMessage(msg, transfer);
      } else {
        worker.postMessage(msg);
      }
    } catch {
      // If the worker is gone, treat this as detached.
      if (this.#workerPort === worker) {
        this.detachWorkerPort(worker);
      }
    }
  }

  #emit(): void {
    const state = this.getState();
    for (const listener of this.#listeners) listener(state);
  }
}
