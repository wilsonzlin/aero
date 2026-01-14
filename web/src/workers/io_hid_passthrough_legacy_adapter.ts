import type { NormalizedHidCollectionInfo } from "../hid/webhid_normalize";
import { computeMaxOutputReportBytesOnWire } from "../hid/hid_report_sizes";
import type {
  HidAttachMessage,
  HidDetachMessage,
  HidFeatureReportResultMessage,
  HidInputReportMessage,
} from "../hid/hid_proxy_protocol";
import type {
  GuestUsbPath,
  GuestUsbPort,
  HidAttachMessage as HidPassthroughAttachMessage,
  HidDetachMessage as HidPassthroughDetachMessage,
  HidFeatureReportResultMessage as HidPassthroughFeatureReportResultMessage,
  HidGetFeatureReportMessage as HidPassthroughGetFeatureReportMessage,
  HidInputReportMessage as HidPassthroughInputReportMessage,
  HidSendReportMessage as HidPassthroughSendReportMessage,
} from "../platform/hid_passthrough_protocol";

const DEFAULT_LEGACY_DEVICE_ID_BASE = 0x4000_0000;

export function computeHasInterruptOut(collections: NormalizedHidCollectionInfo[]): boolean {
  const maxOutputBytes = computeMaxOutputReportBytesOnWire(collections);
  return maxOutputBytes > 0 && maxOutputBytes <= 64;
}

function arrayBufferFromView(view: Uint8Array): ArrayBuffer {
  if (view.buffer instanceof ArrayBuffer) {
    if (view.byteOffset === 0 && view.byteLength === view.buffer.byteLength) return view.buffer;
    return view.buffer.slice(view.byteOffset, view.byteOffset + view.byteLength);
  }

  const out = new Uint8Array(view.byteLength);
  out.set(view);
  return out.buffer;
}

/**
 * Adapter for the legacy `hid:*` passthrough protocol.
 *
 * The legacy protocol uses string device IDs (WebHID `HIDDevice.id`), while the guest-side bridges
 * use numeric IDs. This helper assigns a stable numeric ID to each legacy string ID and can
 * translate messages into the newer `hid.*` protocol.
 */
export class IoWorkerLegacyHidPassthroughAdapter {
  #nextDeviceId: number;

  readonly #numericIdByLegacyId = new Map<string, number>();
  readonly #legacyIdByNumericId = new Map<number, string>();
  readonly #attachedNumericIds = new Set<number>();

  constructor(options: { firstDeviceId?: number } = {}) {
    this.#nextDeviceId = options.firstDeviceId ?? DEFAULT_LEGACY_DEVICE_ID_BASE;
  }

  attach(msg: HidPassthroughAttachMessage): HidAttachMessage {
    const existing = this.#numericIdByLegacyId.get(msg.deviceId);
    const requested = msg.numericDeviceId;
    const requestedValid =
      typeof requested === "number" && Number.isFinite(requested) && Number.isInteger(requested) && requested >= 0 && requested <= 0xffff_ffff;

    let deviceId: number;
    if (existing !== undefined) {
      deviceId = existing;
    } else if (requestedValid && !this.#legacyIdByNumericId.has(requested >>> 0)) {
      deviceId = requested >>> 0;
      this.#nextDeviceId = Math.max(this.#nextDeviceId, deviceId + 1);
      this.#numericIdByLegacyId.set(msg.deviceId, deviceId);
      this.#legacyIdByNumericId.set(deviceId, msg.deviceId);
    } else {
      deviceId = this.#nextDeviceId++;
      this.#numericIdByLegacyId.set(msg.deviceId, deviceId);
      this.#legacyIdByNumericId.set(deviceId, msg.deviceId);
    }

    const guestPath: GuestUsbPath = msg.guestPath ?? [msg.guestPort as GuestUsbPort];
    const guestPort: GuestUsbPort = guestPath[0] as GuestUsbPort;

    this.#attachedNumericIds.add(deviceId);

    return {
      type: "hid.attach",
      deviceId,
      vendorId: msg.vendorId,
      productId: msg.productId,
      ...(msg.productName ? { productName: msg.productName } : {}),
      guestPath,
      guestPort,
      collections: msg.collections,
      hasInterruptOut: computeHasInterruptOut(msg.collections),
    };
  }

  detach(msg: HidPassthroughDetachMessage): HidDetachMessage | null {
    const deviceId = this.#numericIdByLegacyId.get(msg.deviceId);
    if (deviceId === undefined) return null;
    this.#attachedNumericIds.delete(deviceId);
    return { type: "hid.detach", deviceId };
  }

  inputReport(msg: HidPassthroughInputReportMessage): HidInputReportMessage | null {
    const deviceId = this.#numericIdByLegacyId.get(msg.deviceId);
    if (deviceId === undefined) return null;
    if (!this.#attachedNumericIds.has(deviceId)) return null;

    return {
      type: "hid.inputReport",
      deviceId,
      reportId: msg.reportId,
      data: new Uint8Array(msg.data) as Uint8Array<ArrayBuffer>,
    };
  }

  sendReport(payload: { deviceId: number; reportType: "output" | "feature"; reportId: number; data: Uint8Array }): HidPassthroughSendReportMessage | null {
    const legacyId = this.#legacyIdByNumericId.get(payload.deviceId);
    if (!legacyId) return null;

    // USB control transfers use a 16-bit wLength, so a single output/feature report cannot exceed
    // u16::MAX bytes of payload (including the report ID prefix when report IDs are in use).
    // Clamp here so a buggy/malicious guest can't trick the adapter into copying a multi-megabyte
    // SharedArrayBuffer view into an ArrayBuffer just to send it to the main thread.
    const maxPayloadBytes = (payload.reportId >>> 0) === 0 ? 0xffff : 0xfffe;
    const clamped = payload.data.byteLength > maxPayloadBytes ? payload.data.subarray(0, maxPayloadBytes) : payload.data;
    const buffer = arrayBufferFromView(clamped);
    return {
      type: "hid:sendReport",
      deviceId: legacyId,
      reportType: payload.reportType,
      reportId: payload.reportId,
      data: buffer,
    };
  }

  getFeatureReport(payload: { deviceId: number; requestId: number; reportId: number }): HidPassthroughGetFeatureReportMessage | null {
    const legacyId = this.#legacyIdByNumericId.get(payload.deviceId);
    if (!legacyId) return null;
    return {
      type: "hid:getFeatureReport",
      deviceId: legacyId,
      requestId: payload.requestId >>> 0,
      reportId: payload.reportId >>> 0,
    };
  }

  featureReportResult(msg: HidPassthroughFeatureReportResultMessage): HidFeatureReportResultMessage | null {
    const deviceId = this.#numericIdByLegacyId.get(msg.deviceId);
    if (deviceId === undefined) return null;
    // Ignore results for devices that are no longer attached.
    if (!this.#attachedNumericIds.has(deviceId)) return null;

    return {
      type: "hid.featureReportResult",
      deviceId,
      requestId: msg.requestId >>> 0,
      reportId: msg.reportId >>> 0,
      ok: msg.ok,
      ...(msg.ok && msg.data instanceof ArrayBuffer ? { data: new Uint8Array(msg.data) as Uint8Array<ArrayBuffer> } : {}),
      ...(!msg.ok && msg.error !== undefined ? { error: msg.error } : {}),
    };
  }
}
