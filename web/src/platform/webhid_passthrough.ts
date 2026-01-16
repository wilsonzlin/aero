import {
  HID_INPUT_REPORT_RECORD_HEADER_BYTES,
  HID_INPUT_REPORT_RECORD_MAGIC,
  HID_INPUT_REPORT_RECORD_VERSION,
} from "../hid/hid_input_report_ring";
import {
  computeFeatureReportPayloadByteLengths,
  computeInputReportPayloadByteLengths,
  computeOutputReportPayloadByteLengths,
} from "../hid/hid_report_sizes";
import { normalizeCollections, type HidCollectionInfo } from "../hid/webhid_normalize";
import { RingBuffer } from "../ipc/ring_buffer";
import { StatusIndex } from "../runtime/shared_layout";
import { formatOneLineError } from "../text";
import { fnv1a32Hex } from "../utils/fnv1a";
import {
  isHidGetFeatureReportMessage,
  isHidSendReportMessage,
  type GuestUsbPath,
  type GuestUsbRootPort,
  type HidFeatureReportResultMessage,
  type HidPassthroughMessage,
} from "./hid_passthrough_protocol";
import {
  DEFAULT_EXTERNAL_HUB_PORT_COUNT,
  EXTERNAL_HUB_ROOT_PORT,
  UHCI_EXTERNAL_HUB_FIRST_DYNAMIC_PORT,
  WEBUSB_GUEST_ROOT_PORT,
} from "../usb/uhci_external_hub";
import { XHCI_MAX_HUB_PORT_COUNT } from "../hid/xhci_hid_topology";

export interface HidPassthroughTarget {
  postMessage(message: HidPassthroughMessage, transfer?: Transferable[]): void;
}

export { UHCI_ROOT_PORTS, EXTERNAL_HUB_ROOT_PORT, DEFAULT_EXTERNAL_HUB_PORT_COUNT } from "../usb/uhci_external_hub";
// Root port 1 is reserved for the guest-visible WebUSB passthrough device (see `io.worker.ts`).
const DEFAULT_NUMERIC_DEVICE_ID_BASE = 0x4000_0000;

export function getNoFreeGuestUsbPortsMessage(
  options: { externalHubPortCount?: number; reservedExternalHubPorts?: number } = {},
): string {
  const hubPortCount = Math.min(options.externalHubPortCount ?? DEFAULT_EXTERNAL_HUB_PORT_COUNT, XHCI_MAX_HUB_PORT_COUNT);
  const reserved = (() => {
    const requested = options.reservedExternalHubPorts;
    if (typeof requested !== "number" || !Number.isFinite(requested) || !Number.isInteger(requested) || requested <= 0) {
      return 0;
    }
    return Math.max(0, Math.min(hubPortCount, Math.min(255, requested | 0)));
  })();
  const usable = Math.max(0, hubPortCount - reserved);

  if (reserved === 0) {
    return `No free guest USB attachment paths (${hubPortCount} total behind the external hub). Detach an existing device first.`;
  }
  return (
    `No free guest USB attachment paths (` +
    `${usable} usable for WebHID passthrough; ` +
    `${reserved} reserved for synthetic HID; ` +
    `${hubPortCount} total behind the external hub). ` +
    `Detach an existing device first.`
  );
}

export type WebHidPassthroughAttachment = {
  device: HIDDevice;
  deviceId: string;
  guestPath: GuestUsbPath;
};

export type WebHidPassthroughState = {
  supported: boolean;
  knownDevices: HIDDevice[];
  attachedDevices: WebHidPassthroughAttachment[];
};

export type WebHidPassthroughListener = (state: WebHidPassthroughState) => void;

type HidLike = Pick<HID, "getDevices" | "requestDevice" | "addEventListener" | "removeEventListener">;

type HidOutputReportSendTask = () => Promise<void>;
type HidOutputReportSendTaskFactory = () => HidOutputReportSendTask;
type HidQueuedOutputReportSendTask = {
  bytes: number;
  run: HidOutputReportSendTask;
};

function getNavigatorHid(): HidLike | null {
  if (typeof navigator === "undefined") return null;
  const maybe = (navigator as Navigator & { hid?: unknown }).hid;
  if (!maybe) return null;
  return maybe as HidLike;
}

function describeDevice(device: HIDDevice): string {
  const name = device.productName || `device (${device.vendorId.toString(16)}:${device.productId.toString(16)})`;
  return `${name} [${device.vendorId.toString(16).padStart(4, "0")}:${device.productId.toString(16).padStart(4, "0")}]`;
}

type ForgettableHidDevice = HIDDevice & { forget: () => Promise<void> };

function canForgetDevice(device: HIDDevice): device is ForgettableHidDevice {
  // `HIDDevice.forget()` is currently Chromium-specific. Keep the check tolerant so
  // this UI continues to work on browsers that don't yet implement it.
  return typeof (device as unknown as { forget?: unknown }).forget === "function";
}

function supportsHidForget(): boolean {
  const ctor = (globalThis as unknown as { HIDDevice?: { prototype?: { forget?: unknown } } }).HIDDevice;
  return typeof ctor?.prototype?.forget === "function";
}

function getBrowserSiteSettingsUrl(): string {
  // Chromium exposes `chrome://settings/content/siteDetails?site=<origin>`.
  // This isn't standardized, but is still a useful hint for the common WebHID/WebUSB case.
  const origin = (globalThis as unknown as { location?: { origin?: unknown } }).location?.origin;
  const encodedOrigin = typeof origin === "string" ? encodeURIComponent(origin) : "";
  return `chrome://settings/content/siteDetails?site=${encodedOrigin}`;
}

const NOOP_TARGET: HidPassthroughTarget = { postMessage: () => {} };
const UNKNOWN_INPUT_REPORT_HARD_CAP_BYTES = 64;
const UNKNOWN_FEATURE_REPORT_HARD_CAP_BYTES = 4096;
// USB control transfers have a 16-bit `wLength`. When report IDs are in use (reportId != 0), the
// on-wire report includes an extra 1-byte reportId prefix, so the payload must be <= 0xfffe.
const MAX_HID_CONTROL_TRANSFER_BYTES = 0xffff;

function maxHidControlPayloadBytes(reportId: number): number {
  return (reportId >>> 0) === 0 ? MAX_HID_CONTROL_TRANSFER_BYTES : MAX_HID_CONTROL_TRANSFER_BYTES - 1;
}
// WebHID requires per-device serialization. If a device call stalls and the guest keeps sending,
// the per-device queue can otherwise grow without bound. Keep this large enough to absorb bursts,
// but bounded to prevent unbounded memory growth.
const DEFAULT_MAX_PENDING_SENDS_PER_DEVICE = 1024;
const DEFAULT_MAX_PENDING_SEND_BYTES_PER_DEVICE = 4 * 1024 * 1024;
const OUTPUT_SEND_DROP_WARN_INTERVAL_MS = 5000;

function assertPositiveSafeInteger(name: string, value: number): number {
  if (!Number.isSafeInteger(value) || value <= 0) {
    throw new Error(`invalid ${name}: ${value}`);
  }
  return value;
}

export class WebHidPassthroughManager {
  readonly #hid: HidLike | null;
  readonly #target: HidPassthroughTarget;
  readonly #externalHubPortCount: number;
  readonly #reservedExternalHubPorts: number;
  readonly #maxPendingSendsPerDevice: number;
  readonly #maxPendingSendsTotal: number;
  readonly #maxPendingSendBytesPerDevice: number;
  readonly #maxPendingSendBytesTotal: number;

  // WebHID output/feature report sends must be serialized per physical device. Multiple sendReport
  // requests can arrive back-to-back from the I/O worker; queue them per `deviceId` so they execute
  // in guest order without stalling other devices.
  readonly #outputReportQueueByDeviceId = new Map<string, HidQueuedOutputReportSendTask[]>();
  readonly #outputReportRunnerTokenByDeviceId = new Map<string, number>();
  #pendingOutputReportSendTotal = 0;
  #pendingOutputReportSendBytesTotal = 0;
  readonly #pendingOutputReportSendBytesByDeviceId = new Map<string, number>();
  #outputSendDropped = 0;
  readonly #outputSendDroppedByDeviceId = new Map<string, number>();
  readonly #outputSendDropWarnedAtByDeviceId = new Map<string, number>();
  #nextOutputReportRunnerToken = 1;

  #inputReportRing: RingBuffer | null = null;
  #status: Int32Array | null = null;

  #knownDevices: HIDDevice[] = [];
  #attachedDevices: WebHidPassthroughAttachment[] = [];
  readonly #listeners = new Set<WebHidPassthroughListener>();

  readonly #devicePaths = new Map<string, GuestUsbPath>();
  readonly #usedExternalHubPorts = new Set<number>();
  #externalHubAttached = false;
  readonly #inputReportListeners = new Map<string, (event: HIDInputReportEvent) => void>();
  readonly #inputReportExpectedPayloadBytes = new Map<string, Map<number, number>>();
  readonly #featureReportExpectedPayloadBytes = new Map<string, Map<number, number>>();
  readonly #featureReportSizeWarned = new Map<string, Set<string>>();
  readonly #outputReportExpectedPayloadBytes = new Map<string, Map<number, number>>();
  readonly #sendReportSizeWarned = new Map<string, Set<string>>();

  readonly #deviceIds = new WeakMap<HIDDevice, string>();
  #nextDeviceOrdinal = 1;

  readonly #numericDeviceIds = new Map<string, number>();
  #nextNumericDeviceId = DEFAULT_NUMERIC_DEVICE_ID_BASE;

  readonly #onConnect: ((event: Event) => void) | null;
  readonly #onDisconnect: ((event: Event) => void) | null;

  constructor(
    options: {
      hid?: HidLike | null;
      target?: HidPassthroughTarget;
      externalHubPortCount?: number;
      reservedExternalHubPorts?: number;
      /**
       * Maximum number of queued (not yet running) WebHID output/feature tasks per device.
       */
      maxPendingDeviceSends?: number;
      /**
       * Legacy alias for {@link maxPendingDeviceSends}.
       */
      maxPendingSendsPerDevice?: number;
      /**
       * Optional global cap across all devices; defaults to unlimited.
       */
      maxPendingSendsTotal?: number;
      /**
       * Maximum total byte size of queued (not yet running) output/feature report
       * payloads per device.
       *
       * WebHID output/feature reports can be up to 64KiB each (USB control
       * transfers). If a send stalls and the guest keeps producing reports, we
       * must bound retained memory even if the queue length limit is high.
       */
      maxPendingSendBytesPerDevice?: number;
      /**
       * Optional global cap across all devices; defaults to unlimited.
       */
      maxPendingSendBytesTotal?: number;
    } = {},
  ) {
    this.#hid = options.hid ?? getNavigatorHid();
    this.#target = options.target ?? NOOP_TARGET;
    this.#externalHubPortCount = (() => {
      const requested = options.externalHubPortCount;
      if (typeof requested !== "number" || !Number.isInteger(requested) || requested <= 0) {
        return Math.min(DEFAULT_EXTERNAL_HUB_PORT_COUNT, XHCI_MAX_HUB_PORT_COUNT);
      }
      // Root port 0 hosts an external hub that also carries fixed synthetic HID devices on
      // ports 1..=(UHCI_EXTERNAL_HUB_FIRST_DYNAMIC_PORT - 1).
      // Never allow the hub to be configured with fewer downstream ports than that reserved range,
      // otherwise synthetic HID attachments can fail once the runtime hub config is applied.
      return Math.max(
        UHCI_EXTERNAL_HUB_FIRST_DYNAMIC_PORT - 1,
        Math.min(XHCI_MAX_HUB_PORT_COUNT, Math.min(255, requested | 0)),
      );
    })();
    this.#reservedExternalHubPorts = (() => {
      const requested = options.reservedExternalHubPorts;
      const base =
        typeof requested === "number" && Number.isFinite(requested) && Number.isInteger(requested) && requested >= 0
          ? requested
          : UHCI_EXTERNAL_HUB_FIRST_DYNAMIC_PORT - 1;
      // Never allocate passthrough devices in the synthetic-device port range
      // (ports 1..=(UHCI_EXTERNAL_HUB_FIRST_DYNAMIC_PORT - 1)).
      const clamped = Math.max(UHCI_EXTERNAL_HUB_FIRST_DYNAMIC_PORT - 1, base | 0);
      return Math.max(0, Math.min(this.#externalHubPortCount, Math.min(255, clamped)));
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
      maxPending === undefined
        ? DEFAULT_MAX_PENDING_SENDS_PER_DEVICE
        : assertPositiveSafeInteger("maxPendingDeviceSends", maxPending);
    this.#maxPendingSendsTotal =
      options.maxPendingSendsTotal === undefined
        ? Number.POSITIVE_INFINITY
        : assertPositiveSafeInteger("maxPendingSendsTotal", options.maxPendingSendsTotal);
    this.#maxPendingSendBytesPerDevice = (() => {
      const requested = options.maxPendingSendBytesPerDevice;
      if (requested === undefined) return DEFAULT_MAX_PENDING_SEND_BYTES_PER_DEVICE;
      if (!Number.isSafeInteger(requested) || requested <= 0) {
        throw new Error(`invalid maxPendingSendBytesPerDevice: ${requested}`);
      }
      return requested;
    })();
    this.#maxPendingSendBytesTotal = (() => {
      const requested = options.maxPendingSendBytesTotal;
      if (requested === undefined) return Number.POSITIVE_INFINITY;
      if (!Number.isSafeInteger(requested) || requested <= 0) {
        throw new Error(`invalid maxPendingSendBytesTotal: ${requested}`);
      }
      return requested;
    })();

    if (this.#hid) {
      this.#onConnect = () => {
        void this.refreshKnownDevices();
      };

      this.#onDisconnect = (event: Event) => {
        void this.#handleDisconnect(event);
      };

      this.#hid.addEventListener("connect", this.#onConnect);
      this.#hid.addEventListener("disconnect", this.#onDisconnect);
    } else {
      this.#onConnect = null;
      this.#onDisconnect = null;
    }
  }

  destroy(): void {
    if (this.#hid && this.#onConnect && this.#onDisconnect) {
      this.#hid.removeEventListener("connect", this.#onConnect);
      this.#hid.removeEventListener("disconnect", this.#onDisconnect);
    }
    for (const attachment of this.#attachedDevices) {
      const listener = this.#inputReportListeners.get(attachment.deviceId);
      if (!listener) continue;
      try {
        attachment.device.removeEventListener("inputreport", listener);
      } catch {
        // Best-effort cleanup only.
      }
    }
    this.#inputReportListeners.clear();
    this.#inputReportExpectedPayloadBytes.clear();
    this.#featureReportExpectedPayloadBytes.clear();
    this.#featureReportSizeWarned.clear();
    this.#outputReportExpectedPayloadBytes.clear();
    this.#sendReportSizeWarned.clear();
    this.#outputReportQueueByDeviceId.clear();
    this.#outputReportRunnerTokenByDeviceId.clear();
    this.#pendingOutputReportSendTotal = 0;
    this.#pendingOutputReportSendBytesTotal = 0;
    this.#pendingOutputReportSendBytesByDeviceId.clear();
    this.#outputSendDropped = 0;
    this.#outputSendDroppedByDeviceId.clear();
    this.#outputSendDropWarnedAtByDeviceId.clear();
    this.#listeners.clear();
  }

  #warnOutputSendDrop(deviceId: string): void {
    const now = typeof performance !== "undefined" ? performance.now() : Date.now();
    const lastWarn = this.#outputSendDropWarnedAtByDeviceId.get(deviceId);
    if (lastWarn !== undefined && now - lastWarn < OUTPUT_SEND_DROP_WARN_INTERVAL_MS) return;
    this.#outputSendDropWarnedAtByDeviceId.set(deviceId, now);

    const dropped = this.#outputSendDroppedByDeviceId.get(deviceId) ?? 0;
    const pending = this.#outputReportQueueByDeviceId.get(deviceId)?.length ?? 0;
    const pendingBytes = this.#pendingOutputReportSendBytesByDeviceId.get(deviceId) ?? 0;
    console.warn(
      `[webhid] Dropping queued HID report tasks for deviceId=${deviceId} (pending=${pending} pendingBytes=${pendingBytes} maxPendingDeviceSends=${this.#maxPendingSendsPerDevice} maxPendingSendBytesPerDevice=${this.#maxPendingSendBytesPerDevice} dropped=${dropped})`,
    );
  }

  #recordOutputSendDrop(deviceId: string): void {
    this.#outputSendDropped += 1;
    this.#outputSendDroppedByDeviceId.set(deviceId, (this.#outputSendDroppedByDeviceId.get(deviceId) ?? 0) + 1);
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

  #enqueueOutputReportSend(deviceId: string, taskBytes: number, createTask: HidOutputReportSendTaskFactory): boolean {
    // Normalise and sanity-check byte accounting; callers should pass the amount of memory the queued
    // task will retain (e.g. the report payload length).
    const bytes = (() => {
      if (!Number.isFinite(taskBytes) || taskBytes < 0) return 0;
      return Math.floor(taskBytes) >>> 0;
    })();
    const queueLen = this.#outputReportQueueByDeviceId.get(deviceId)?.length ?? 0;
    const queueBytes = this.#pendingOutputReportSendBytesByDeviceId.get(deviceId) ?? 0;
    // Drop policy: drop newest when at/over the cap. This preserves FIFO ordering for already-queued
    // reports and keeps memory bounded when the guest spams reports or a WebHID Promise never resolves.
    if (
      queueLen >= this.#maxPendingSendsPerDevice ||
      this.#pendingOutputReportSendTotal >= this.#maxPendingSendsTotal ||
      bytes > this.#maxPendingSendBytesPerDevice ||
      queueBytes + bytes > this.#maxPendingSendBytesPerDevice ||
      bytes > this.#maxPendingSendBytesTotal ||
      this.#pendingOutputReportSendBytesTotal + bytes > this.#maxPendingSendBytesTotal
    ) {
      this.#recordOutputSendDrop(deviceId);
      return false;
    }

    let queue = this.#outputReportQueueByDeviceId.get(deviceId);
    if (!queue) {
      queue = [];
      this.#outputReportQueueByDeviceId.set(deviceId, queue);
    }
    // Only create the task after we know it will be queued so we don't eagerly copy payload buffers
    // (e.g. large postMessage payloads) just to immediately drop them.
    queue.push({ bytes, run: createTask() });
    this.#pendingOutputReportSendTotal += 1;
    this.#pendingOutputReportSendBytesTotal += bytes;
    this.#pendingOutputReportSendBytesByDeviceId.set(deviceId, queueBytes + bytes);
    if (this.#outputReportRunnerTokenByDeviceId.has(deviceId)) return true;
    const token = this.#nextOutputReportRunnerToken++;
    this.#outputReportRunnerTokenByDeviceId.set(deviceId, token);
    void this.#runOutputReportSendQueue(deviceId, token);
    return true;
  }

  #dequeueOutputReportSend(deviceId: string): HidOutputReportSendTask | null {
    const queue = this.#outputReportQueueByDeviceId.get(deviceId);
    if (!queue || queue.length === 0) return null;
    const task = queue.shift()!;
    this.#pendingOutputReportSendTotal = Math.max(0, this.#pendingOutputReportSendTotal - 1);
    this.#pendingOutputReportSendBytesTotal = Math.max(0, this.#pendingOutputReportSendBytesTotal - task.bytes);
    const nextBytes = (this.#pendingOutputReportSendBytesByDeviceId.get(deviceId) ?? 0) - task.bytes;
    if (nextBytes > 0) {
      this.#pendingOutputReportSendBytesByDeviceId.set(deviceId, nextBytes);
    } else {
      this.#pendingOutputReportSendBytesByDeviceId.delete(deviceId);
    }
    if (queue.length === 0) {
      this.#outputReportQueueByDeviceId.delete(deviceId);
      this.#pendingOutputReportSendBytesByDeviceId.delete(deviceId);
    }
    return task.run;
  }

  async #runOutputReportSendQueue(deviceId: string, token: number): Promise<void> {
    try {
      // eslint-disable-next-line no-constant-condition
      while (true) {
        if (this.#outputReportRunnerTokenByDeviceId.get(deviceId) !== token) break;
        const task = this.#dequeueOutputReportSend(deviceId);
        if (!task) break;
        try {
          await task();
        } catch (err) {
          console.warn("WebHID output report send task failed", err);
        }
      }
    } finally {
      if (this.#outputReportRunnerTokenByDeviceId.get(deviceId) === token) {
        this.#outputReportRunnerTokenByDeviceId.delete(deviceId);
      }
    }
  }

  /**
   * Handle I/O worker messages destined for the WebHID broker.
   *
   * This is intentionally best-effort: it must never throw synchronously because
   * it's often wired directly to a `Worker#message` event.
   */
  handleWorkerMessage(msg: unknown): void {
    if (isHidSendReportMessage(msg)) {
      const attachment = this.#attachedDevices.find((d) => d.deviceId === msg.deviceId);
      if (!attachment) return;

      const deviceId = msg.deviceId;
      const reportType = msg.reportType;
      const reportId = msg.reportId;
      const srcLen = msg.data.byteLength;
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

      this.#enqueueOutputReportSend(deviceId, destLen, () => {
        const warned = this.#sendReportSizeWarned.get(deviceId);
        const warnOnce = (key: string, message: string): void => {
          if (!warned) {
            console.warn(message);
            return;
          }
          if (warned.has(key)) return;
          warned.add(key);
          console.warn(message);
        };
        if (warnKind) {
          warnOnce(`${reportType}:${reportId}:${warnKind}`, warnMessage);
        }

        const bytes = new Uint8Array(msg.data);
        let dataToSend: Uint8Array<ArrayBuffer>;
        if (destLen === srcLen) {
          dataToSend = bytes as Uint8Array<ArrayBuffer>;
        } else {
          const copyLen = Math.min(srcLen, destLen);
          const out = new Uint8Array(destLen);
          out.set(bytes.subarray(0, copyLen));
          dataToSend = out as Uint8Array<ArrayBuffer>;
        }

        return async () => {
          const current = this.#attachedDevices.find((d) => d.deviceId === deviceId);
          if (!current) return;
          const device = current.device;
          try {
            if (reportType === "feature") {
              await device.sendFeatureReport(reportId, dataToSend);
            } else {
              await device.sendReport(reportId, dataToSend);
            }
          } catch (err) {
            console.warn(`WebHID ${reportType === "feature" ? "sendFeatureReport" : "sendReport"}() failed`, err);
          }
        };
      });
      return;
    }

    if (isHidGetFeatureReportMessage(msg)) {
      const deviceId = msg.deviceId;
      const reportId = msg.reportId;
      const requestId = msg.requestId;

      const reply = (res: HidFeatureReportResultMessage, transfer?: Transferable[]): void => {
        try {
          if (transfer) {
            this.#target.postMessage(res, transfer);
          } else {
            this.#target.postMessage(res);
          }
        } catch {
          // Best-effort only; if the worker is gone we'll naturally stop receiving requests.
        }
      };

      const attachment = this.#attachedDevices.find((d) => d.deviceId === deviceId);
      if (!attachment) {
        reply({
          type: "hid:featureReportResult",
          deviceId,
          requestId,
          reportId,
          ok: false,
          error: `DeviceId=${deviceId} is not attached.`,
        });
        return;
      }

      // Serialize receiveFeatureReport relative to sendReport/sendFeatureReport calls for the same
      // physical device, matching WebHID ordering expectations.
      const queued = this.#enqueueOutputReportSend(deviceId, 0, () => async () => {
        const current = this.#attachedDevices.find((d) => d.deviceId === deviceId);
        if (!current) {
          reply({
            type: "hid:featureReportResult",
            deviceId,
            requestId,
            reportId,
            ok: false,
            error: `DeviceId=${deviceId} is not attached.`,
          });
          return;
        }

        try {
          const view = await current.device.receiveFeatureReport(reportId);
          if (!(view instanceof DataView)) {
            reply({
              type: "hid:featureReportResult",
              deviceId,
              requestId,
              reportId,
              ok: false,
              error: "receiveFeatureReport returned non-DataView value.",
            });
            return;
          }

          const srcLen = view.byteLength;
          const expected = this.#featureReportExpectedPayloadBytes.get(deviceId)?.get(reportId);
          const warned = this.#featureReportSizeWarned.get(deviceId);
          const warnOnce = (key: string, message: string): void => {
            if (!warned) {
              console.warn(message);
              return;
            }
            if (warned.has(key)) return;
            warned.add(key);
            console.warn(message);
          };

          let destLen: number;
          if (expected !== undefined) {
            destLen = expected;
            if (srcLen > expected) {
              warnOnce(
                `${reportId}:truncated`,
                `[webhid] feature report length mismatch (deviceId=${deviceId} reportId=${reportId} expected=${expected} got=${srcLen}); truncating`,
              );
            } else if (srcLen < expected) {
              warnOnce(
                `${reportId}:padded`,
                `[webhid] feature report length mismatch (deviceId=${deviceId} reportId=${reportId} expected=${expected} got=${srcLen}); zero-padding`,
              );
            }
          } else if (srcLen > UNKNOWN_FEATURE_REPORT_HARD_CAP_BYTES) {
            destLen = UNKNOWN_FEATURE_REPORT_HARD_CAP_BYTES;
            warnOnce(
              `${reportId}:hardCap`,
              `[webhid] feature report reportId=${reportId} for deviceId=${deviceId} has unknown expected size; capping ${srcLen} bytes to ${UNKNOWN_FEATURE_REPORT_HARD_CAP_BYTES}`,
            );
          } else {
            destLen = srcLen;
          }

          const copyLen = Math.min(srcLen, destLen);
          const src = new Uint8Array(view.buffer, view.byteOffset, copyLen);
          const data = new Uint8Array(destLen);
          data.set(src);
          const res: HidFeatureReportResultMessage = {
            type: "hid:featureReportResult",
            deviceId,
            requestId,
            reportId,
            ok: true,
            data: data.buffer,
          };
          reply(res, [data.buffer]);
        } catch (err) {
          const message = formatOneLineError(err, 512);
          reply({
            type: "hid:featureReportResult",
            deviceId,
            requestId,
            reportId,
            ok: false,
            error: message,
          });
        }
      });
      if (!queued) {
        reply({
          type: "hid:featureReportResult",
          deviceId,
          requestId,
          reportId,
          ok: false,
          error: "Too many pending HID report tasks for this device.",
        });
      }
    }
  }

  setInputReportRing(ring: RingBuffer | null, status: Int32Array | null = null): void {
    if (ring && ring !== this.#inputReportRing) {
      ring.reset();
    }
    this.#inputReportRing = ring;
    this.#status = status;
  }

  /**
   * Re-send `hid:*` attach messages for already-attached devices.
   *
   * This is useful when the I/O worker is restarted or replaced: the page retains
   * WebHID device permissions, but the new worker needs to rebuild guest-side
   * state (virtual hubs, passthrough bridges, etc).
   */
  async resyncAttachedDevices(): Promise<void> {
    if (this.#target === NOOP_TARGET) return;
    if (this.#attachedDevices.length === 0) return;

    // Treat the new worker as a fresh guest session.
    this.#externalHubAttached = false;

    const needsHub = this.#attachedDevices.some((entry) => entry.guestPath[0] === EXTERNAL_HUB_ROOT_PORT && entry.guestPath.length >= 2);
    if (needsHub) {
      try {
        this.#ensureExternalHubAttached();
      } catch (err) {
        console.warn("WebHID passthrough attachHub failed during resync", err);
      }
    }

    for (const entry of this.#attachedDevices) {
      const device = entry.device;
      const deviceId = entry.deviceId;
      const guestPath = entry.guestPath;
      const guestPort = guestPath[0] as GuestUsbRootPort;
      const numericDeviceId = this.#numericDeviceIdFor(deviceId);

      try {
        // Devices should already be opened by `attachKnownDevice`, but keep this
        // best-effort in case a browser unexpectedly closed them while the worker
        // was down.
        const res = device.open();
        void res.catch(() => undefined);
      } catch {
        // Best-effort; proceed anyway.
      }

      let normalizedCollections: ReturnType<typeof normalizeCollections>;
      try {
        const rawCollections = (device as unknown as { collections?: unknown }).collections;
        normalizedCollections = normalizeCollections(
          (rawCollections ?? []) as unknown as readonly HidCollectionInfo[],
          { validate: true },
        );
      } catch (err) {
        console.warn("WebHID passthrough collection normalization failed during resync", err);
        continue;
      }

      try {
        this.#target.postMessage({
          type: "hid:attach",
          deviceId,
          numericDeviceId,
          guestPort,
          guestPath,
          vendorId: device.vendorId,
          productId: device.productId,
          ...(device.productName ? { productName: device.productName } : {}),
          collections: normalizedCollections,
        });
      } catch (err) {
        console.warn("WebHID passthrough attach failed during resync", err);
      }
    }
  }

  getState(): WebHidPassthroughState {
    return {
      supported: !!this.#hid,
      knownDevices: this.#knownDevices,
      attachedDevices: this.#attachedDevices,
    };
  }

  subscribe(listener: WebHidPassthroughListener): () => void {
    this.#listeners.add(listener);
    listener(this.getState());
    return () => {
      this.#listeners.delete(listener);
    };
  }

  async refreshKnownDevices(): Promise<void> {
    if (!this.#hid) {
      this.#knownDevices = [];
      this.#emit();
      return;
    }

    try {
      this.#knownDevices = await this.#hid.getDevices();
    } catch (err) {
      // Browsers may throw when WebHID is disabled by policy/flags. Treat this as
      // "supported but unavailable" rather than crashing the UI.
      console.warn("WebHID getDevices() failed", err);
      this.#knownDevices = [];
    }

    this.#emit();
  }

  async requestAndAttachDevice(filters: HIDDeviceFilter[] = []): Promise<void> {
    if (!this.#hid) {
      throw new Error("WebHID is unavailable in this browser.");
    }

    const devices = await this.#hid.requestDevice({ filters });
    for (const device of devices) {
      await this.attachKnownDevice(device);
    }

    // `requestDevice()` also grants permissions; refresh so the "known devices"
    // list stays in sync across browsers.
    await this.refreshKnownDevices();
  }

  /**
   * Convenience wrapper for the runtime UI: request device permission with no
   * filters and attach all selected devices.
   */
  async requestAndAttach(): Promise<void> {
    await this.requestAndAttachDevice([]);
  }

  async attachKnownDevice(device: HIDDevice): Promise<void> {
    const deviceId = this.#deviceIdFor(device);
    if (this.#devicePaths.has(deviceId)) {
      return;
    }

    const guestPath = this.#allocatePath();
    if (!guestPath) {
      throw new Error(
        getNoFreeGuestUsbPortsMessage({
          externalHubPortCount: this.#externalHubPortCount,
          reservedExternalHubPorts: this.#reservedExternalHubPorts,
        }),
      );
    }

    if (guestPath[0] === EXTERNAL_HUB_ROOT_PORT && guestPath.length >= 2) {
      this.#ensureExternalHubAttached();
    }

    await device.open();

    if (this.#target !== NOOP_TARGET) {
      const numericDeviceId = this.#numericDeviceIdFor(deviceId);
      let normalizedCollections: ReturnType<typeof normalizeCollections>;
      let inputReportPayloadBytes: Map<number, number>;
      let featureReportPayloadBytes: Map<number, number>;
      let outputReportPayloadBytes: Map<number, number>;
      try {
        const rawCollections = (device as unknown as { collections?: unknown }).collections;
        normalizedCollections = normalizeCollections(
          (rawCollections ?? []) as unknown as readonly HidCollectionInfo[],
          { validate: true },
        );
        inputReportPayloadBytes = computeInputReportPayloadByteLengths(normalizedCollections);
        featureReportPayloadBytes = computeFeatureReportPayloadByteLengths(normalizedCollections);
        outputReportPayloadBytes = computeOutputReportPayloadByteLengths(normalizedCollections);
      } catch (err) {
        try {
          await device.close();
        } catch {
          // Ignore close failures when attach fails.
        }
        throw err;
      }

      try {
        this.#target.postMessage({
          type: "hid:attach",
          deviceId,
          numericDeviceId,
          guestPort: guestPath[0] as GuestUsbRootPort,
          guestPath,
          vendorId: device.vendorId,
          productId: device.productId,
          ...(device.productName ? { productName: device.productName } : {}),
          collections: normalizedCollections,
        });
      } catch (err) {
        try {
          await device.close();
        } catch {
          // Ignore close failures when attach fails.
        }
        throw err;
      }

      this.#inputReportExpectedPayloadBytes.set(deviceId, inputReportPayloadBytes);
      this.#featureReportExpectedPayloadBytes.set(deviceId, featureReportPayloadBytes);
      this.#featureReportSizeWarned.set(deviceId, new Set());
      this.#outputReportExpectedPayloadBytes.set(deviceId, outputReportPayloadBytes);
      this.#sendReportSizeWarned.set(deviceId, new Set());
      const expectedInputPayloadBytes = inputReportPayloadBytes;
      const warned = new Set<string>();
      const warnOnce = (key: string, message: string): void => {
        if (warned.has(key)) return;
        warned.add(key);
        console.warn(message);
      };

      const onInputReport = (event: HIDInputReportEvent): void => {
        try {
          const view = event.data;
          if (!(view instanceof DataView)) return;

          const rawReportId = (event as unknown as { reportId?: unknown }).reportId;
          if (
            rawReportId !== undefined &&
            (typeof rawReportId !== "number" || !Number.isInteger(rawReportId) || rawReportId < 0 || rawReportId > 0xff)
          ) {
            warnOnce(
              "invalidReportId",
              `[webhid] inputreport has invalid reportId=${String(rawReportId)} for deviceId=${deviceId}; dropping`,
            );
            return;
          }
          const reportId = (rawReportId === undefined ? 0 : rawReportId) >>> 0;
          const srcLen = view.byteLength;

          const expected = expectedInputPayloadBytes.get(reportId);
          let destLen: number;
          if (expected !== undefined) {
            destLen = expected;
            if (srcLen > expected) {
              warnOnce(
                `${reportId}:truncated`,
                `[webhid] inputreport length mismatch (deviceId=${deviceId} reportId=${reportId} expected=${expected} got=${srcLen}); truncating`,
              );
            } else if (srcLen < expected) {
              warnOnce(
                `${reportId}:padded`,
                `[webhid] inputreport length mismatch (deviceId=${deviceId} reportId=${reportId} expected=${expected} got=${srcLen}); zero-padding`,
              );
            }
          } else if (srcLen > UNKNOWN_INPUT_REPORT_HARD_CAP_BYTES) {
            destLen = UNKNOWN_INPUT_REPORT_HARD_CAP_BYTES;
            warnOnce(
              `${reportId}:hardCap`,
              `[webhid] inputreport reportId=${reportId} for deviceId=${deviceId} has unknown expected size; capping ${srcLen} bytes to ${UNKNOWN_INPUT_REPORT_HARD_CAP_BYTES}`,
            );
          } else {
            destLen = srcLen;
          }

          // Only ever create a view over the clamped amount of input data so a bogus
          // (or malicious) browser/device can't trick us into copying huge buffers.
          const copyLen = Math.min(srcLen, destLen);
          const src = new Uint8Array(view.buffer, view.byteOffset, copyLen);
          const ring = this.#inputReportRing;
          if (ring && this.#canUseSharedMemory()) {
            const ts = (event as unknown as { timeStamp?: unknown }).timeStamp;
            const tsMs = typeof ts === "number" ? (Math.max(0, Math.floor(ts)) >>> 0) : 0;
            let ok = false;
            try {
              ok = ring.tryPushWithWriterSpsc(HID_INPUT_REPORT_RECORD_HEADER_BYTES + destLen, (dest) => {
                const dv = new DataView(dest.buffer, dest.byteOffset, dest.byteLength);
                dv.setUint32(0, HID_INPUT_REPORT_RECORD_MAGIC, true);
                dv.setUint32(4, HID_INPUT_REPORT_RECORD_VERSION, true);
                dv.setUint32(8, numericDeviceId >>> 0, true);
                dv.setUint32(12, reportId, true);
                dv.setUint32(16, tsMs, true);
                dv.setUint32(20, destLen >>> 0, true);
                const payload = dest.subarray(HID_INPUT_REPORT_RECORD_HEADER_BYTES);
                payload.set(src);
                if (copyLen < destLen) payload.fill(0, copyLen);
              });
            } catch (err) {
              // Ring corruption should not wedge the main thread; disable the SAB fast path and
              // fall back to per-report postMessage forwarding.
              console.warn("[webhid] input report ring push failed; disabling ring fast path", err);
              this.#inputReportRing = null;
              this.#status = null;
            }
            if (ok && this.#inputReportRing === ring) return;
            if (this.#inputReportRing === ring) {
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
          }

          const out = new Uint8Array(destLen);
          out.set(src);
          const data = out.buffer;
          this.#target.postMessage({ type: "hid:inputReport", deviceId, reportId, data }, [data]);
        } catch (err) {
          console.warn("WebHID inputreport forwarding failed", err);
        }
      };
      try {
        device.addEventListener("inputreport", onInputReport);
        this.#inputReportListeners.set(deviceId, onInputReport);
      } catch (err) {
        console.warn("WebHID addEventListener(inputreport) failed", err);
      }
    }

    this.#devicePaths.set(deviceId, guestPath);
    this.#trackAllocatedPath(guestPath);
    this.#attachedDevices = [...this.#attachedDevices, { device, deviceId, guestPath }].sort((a, b) =>
      compareGuestPaths(a.guestPath, b.guestPath),
    );
    this.#emit();
  }

  async detachDevice(device: HIDDevice): Promise<void> {
    const deviceId = this.#deviceIds.get(device);
    if (!deviceId) return;
    const pending = this.#outputReportQueueByDeviceId.get(deviceId);
    if (pending) {
      this.#pendingOutputReportSendTotal = Math.max(0, this.#pendingOutputReportSendTotal - pending.length);
      const bytes = this.#pendingOutputReportSendBytesByDeviceId.get(deviceId) ?? 0;
      this.#pendingOutputReportSendBytesTotal = Math.max(0, this.#pendingOutputReportSendBytesTotal - bytes);
      this.#pendingOutputReportSendBytesByDeviceId.delete(deviceId);
    }
    this.#outputReportQueueByDeviceId.delete(deviceId);
    this.#outputReportRunnerTokenByDeviceId.delete(deviceId);
    this.#outputSendDroppedByDeviceId.delete(deviceId);
    this.#outputSendDropWarnedAtByDeviceId.delete(deviceId);
    this.#inputReportExpectedPayloadBytes.delete(deviceId);
    this.#featureReportExpectedPayloadBytes.delete(deviceId);
    this.#featureReportSizeWarned.delete(deviceId);
    this.#outputReportExpectedPayloadBytes.delete(deviceId);
    this.#sendReportSizeWarned.delete(deviceId);

    const listener = this.#inputReportListeners.get(deviceId);
    if (listener) {
      try {
        device.removeEventListener("inputreport", listener);
      } catch (err) {
        console.warn("WebHID removeEventListener(inputreport) failed", err);
      }
      this.#inputReportListeners.delete(deviceId);
    }

    const guestPath = this.#devicePaths.get(deviceId);
    if (!guestPath) return;

    let detachError: unknown | null = null;
    if (this.#target !== NOOP_TARGET) {
      try {
        this.#target.postMessage({
          type: "hid:detach",
          deviceId,
          guestPort: guestPath[0] as GuestUsbRootPort,
          guestPath,
        });
      } catch (err) {
        detachError = err;
      }
    }

    this.#devicePaths.delete(deviceId);
    this.#untrackAllocatedPath(guestPath);
    this.#attachedDevices = this.#attachedDevices.filter((d) => d.deviceId !== deviceId);
    this.#emit();

    try {
      await device.close();
    } catch (err) {
      console.warn("WebHID device.close() failed", err);
    }

    if (detachError) {
      throw detachError;
    }
  }

  #emit(): void {
    const state = this.getState();
    for (const listener of this.#listeners) listener(state);
  }

  #ensureExternalHubAttached(): void {
    if (this.#externalHubAttached) return;
    this.#target.postMessage({ type: "hid:attachHub", guestPath: [EXTERNAL_HUB_ROOT_PORT], portCount: this.#externalHubPortCount });
    this.#externalHubAttached = true;
  }

  #allocatePath(): GuestUsbPath | null {
    // Prefer attaching behind the emulated external hub (root port 0).
    for (
      let hubPort = this.#reservedExternalHubPorts + 1;
      hubPort <= this.#externalHubPortCount;
      hubPort += 1
    ) {
      if (this.#usedExternalHubPorts.has(hubPort)) continue;
      return [EXTERNAL_HUB_ROOT_PORT, hubPort];
    }

    return null;
  }

  #trackAllocatedPath(path: GuestUsbPath): void {
    if (path[0] === EXTERNAL_HUB_ROOT_PORT && path.length >= 2) {
      const hubPort = path[1]!;
      this.#usedExternalHubPorts.add(hubPort);
    }
  }

  #untrackAllocatedPath(path: GuestUsbPath): void {
    if (path[0] === EXTERNAL_HUB_ROOT_PORT && path.length >= 2) {
      const hubPort = path[1]!;
      this.#usedExternalHubPorts.delete(hubPort);
    }
  }

  #deviceIdFor(device: HIDDevice): string {
    const existing = this.#deviceIds.get(device);
    if (existing) return existing;

    const base = `${device.vendorId}:${device.productId}:${device.productName ?? ""}`;
    const hash = fnv1a32Hex(new TextEncoder().encode(base));
    const id = `${hash}-${this.#nextDeviceOrdinal++}`;
    this.#deviceIds.set(device, id);
    return id;
  }

  #numericDeviceIdFor(deviceId: string): number {
    const existing = this.#numericDeviceIds.get(deviceId);
    if (existing !== undefined) return existing;

    const id = this.#nextNumericDeviceId++ >>> 0;
    this.#numericDeviceIds.set(deviceId, id);
    return id;
  }

  #canUseSharedMemory(): boolean {
    // SharedArrayBuffer requires cross-origin isolation in browsers. Node/Vitest may still provide it,
    // but keep the check aligned with the browser contract so behaviour matches production.
    if ((globalThis as unknown as { crossOriginIsolated?: unknown }).crossOriginIsolated !== true) return false;
    if (typeof SharedArrayBuffer === "undefined") return false;
    if (typeof Atomics === "undefined") return false;
    return true;
  }

  async #handleDisconnect(event: Event): Promise<void> {
    const dev = (event as unknown as HIDConnectionEvent).device;
    if (dev) {
      try {
        await this.detachDevice(dev);
      } catch {
        // Ignore detach failures on disconnect.
      }
    }
    await this.refreshKnownDevices();
  }

  getExternalHubPortCount(): number {
    return this.#externalHubPortCount;
  }

  getReservedExternalHubPorts(): number {
    return this.#reservedExternalHubPorts;
  }
}

function compareGuestPaths(a: GuestUsbPath, b: GuestUsbPath): number {
  const len = Math.min(a.length, b.length);
  for (let i = 0; i < len; i += 1) {
    const diff = (a[i] ?? 0) - (b[i] ?? 0);
    if (diff !== 0) return diff;
  }
  return a.length - b.length;
}

function formatGuestPath(path: GuestUsbPath): string {
  return path.join(".");
}

function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  props: Record<string, unknown> = {},
  ...children: Array<HTMLElement | null | undefined>
): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  for (const [key, value] of Object.entries(props)) {
    if (value === undefined) continue;
    if (key === "class") {
      node.className = String(value);
    } else if (key === "text") {
      node.textContent = String(value);
    } else if (key.startsWith("on") && typeof value === "function") {
      (node as unknown as Record<string, unknown>)[key.toLowerCase()] = value;
    } else {
      node.setAttribute(key, String(value));
    }
  }
  for (const child of children) {
    if (!child) continue;
    node.append(child);
  }
  return node;
}

/**
 * Minimal passthrough UI for debugging / manual testing.
 *
 * Note: Unit tests run in the `node` environment. The DOM interactions are
 * intentionally simple so tests can stub out `document.createElement`.
 */
export function mountWebHidPassthroughPanel(host: HTMLElement, manager: WebHidPassthroughManager): () => void {
  const hubPortCount = manager.getExternalHubPortCount();
  const reservedPorts = manager.getReservedExternalHubPorts();
  const portHint = el("div", {
    class: "mono",
    text:
      `Guest USB root port ${EXTERNAL_HUB_ROOT_PORT} hosts an emulated external USB hub (${hubPortCount} ports). ` +
      (reservedPorts > 0
         ? `Ports 1..=${reservedPorts} are reserved for synthetic HID devices (keyboard/mouse/gamepad/consumer-control). ` +
           `Passthrough devices attach behind it using paths like ${EXTERNAL_HUB_ROOT_PORT}.${reservedPorts + 1}. `
         : `Passthrough devices attach behind it using paths like ${EXTERNAL_HUB_ROOT_PORT}.${UHCI_EXTERNAL_HUB_FIRST_DYNAMIC_PORT}. `) +
      `Guest USB root port ${WEBUSB_GUEST_ROOT_PORT} is reserved for the guest-visible WebUSB passthrough device.`,
  });

  const permissionHint = el("div", { class: "mono" });
  const siteSettingsLink = el("a", {
    href: getBrowserSiteSettingsUrl(),
    target: "_blank",
    rel: "noopener",
    text: "site settings",
  });

  const error = el("pre", { text: "" });

  const forgetDevice = async (device: HIDDevice): Promise<void> => {
    error.textContent = "";
    const errors: string[] = [];

    const attached = manager.getState().attachedDevices.some((d) => d.device === device);
    if (attached) {
      try {
        await manager.detachDevice(device);
      } catch (err) {
        errors.push(`Detach failed: ${formatOneLineError(err, 512)}`);
      }
    }

    try {
      await (device as ForgettableHidDevice).forget();
    } catch (err) {
      errors.push(`Forget failed: ${formatOneLineError(err, 512)}`);
    }

    await manager.refreshKnownDevices();

    if (errors.length) {
      error.textContent = errors.join("\n");
    }
  };

  const requestButton = el("button", {
    text: "Request device…",
    onclick: async () => {
      error.textContent = "";
      try {
        await manager.requestAndAttachDevice([]);
      } catch (err) {
        error.textContent = formatOneLineError(err, 512);
      }
    },
  }) as HTMLButtonElement;

  const knownList = el("ul");
  const attachedList = el("ul");

  function render(state: WebHidPassthroughState): void {
    requestButton.disabled = !state.supported;

    if (!state.supported) {
      knownList.replaceChildren(el("li", { text: "WebHID is not available in this browser/context." }));
      attachedList.replaceChildren(el("li", { text: "No devices attached." }));
      permissionHint.textContent = "";
      return;
    }

    const forgetSupported =
      supportsHidForget() ||
      state.knownDevices.some((d) => canForgetDevice(d)) ||
      state.attachedDevices.some((d) => canForgetDevice(d.device));

    const hintPrefix = forgetSupported
      ? "WebHID permissions persist per-origin. Some Chromium builds support revoking permissions via the “Forget” buttons below; otherwise, use your browser's "
      : "WebHID permissions persist per-origin. To revoke access, use your browser's ";
    permissionHint.replaceChildren(
      el("span", { text: hintPrefix }),
      siteSettingsLink,
      el("span", { text: " and remove HID device permissions for this site." }),
    );

    const attachedSet = new Set(state.attachedDevices.map((d) => d.device));
    const known = state.knownDevices.filter((d) => !attachedSet.has(d));

    knownList.replaceChildren(
      ...(known.length
        ? known.map((device) =>
            el(
              "li",
              {},
              el("span", { text: describeDevice(device) }),
              el("button", {
                text: "Attach",
                onclick: async () => {
                  error.textContent = "";
                  try {
                    await manager.attachKnownDevice(device);
                  } catch (err) {
                    error.textContent = formatOneLineError(err, 512);
                  }
                },
              }),
              canForgetDevice(device)
                ? el("button", {
                    text: "Forget",
                    onclick: async () => {
                      await forgetDevice(device);
                    },
                  })
                : null,
            ),
          )
        : [el("li", { text: "No known devices. Use “Request device…” to grant access." })]),
    );

    attachedList.replaceChildren(
      ...(state.attachedDevices.length
        ? state.attachedDevices.map((attachment) => {
            const device = attachment.device;
            return el(
              "li",
              {},
              el("span", { class: "mono", text: `path=${formatGuestPath(attachment.guestPath)}` }),
              el("span", { text: ` ${describeDevice(device)}` }),
              el("button", {
                text: "Detach",
                onclick: async () => {
                  error.textContent = "";
                  try {
                    await manager.detachDevice(device);
                  } catch (err) {
                    error.textContent = formatOneLineError(err, 512);
                  }
                },
              }),
              canForgetDevice(device)
                ? el("button", {
                    text: "Forget",
                    onclick: async () => {
                      await forgetDevice(device);
                    },
                  })
                : null,
            );
          })
        : [el("li", { text: "No devices attached." })]),
    );
  }

  const unsubscribe = manager.subscribe(render);

  host.replaceChildren(
    el("h3", { text: "WebHID passthrough (USB HID → guest UHCI)" }),
    portHint,
    permissionHint,
    el("div", { class: "row" }, requestButton),
    el("h4", { text: "Known devices" }),
    knownList,
    el("h4", { text: "Attached devices" }),
    attachedList,
    error,
  );

  void manager.refreshKnownDevices();

  return () => {
    unsubscribe();
    manager.destroy();
  };
}
